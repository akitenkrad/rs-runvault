//! `runvault query --refresh` — the flattened index the SQL reads.
//!
//! Thousands of runs cannot be read out of their JSON on every question, so the
//! aggregation repository is walked once and reduced to seven parquet tables
//! (design note §5.2). The index is a derived thing: it is not tracked by git,
//! and deleting it costs nothing but the walk.
//!
//! `schema/v1/index.columns.json` is the specification, and it is read here
//! rather than transcribed: the table definitions, the column order and the
//! insert order all come from that file, so a column cannot exist in the SQL
//! examples and be missing from the writer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runvault::legacy::{self, LegacyRun};
use runvault::meta::RunMeta;
use runvault::status::{RunStatus, State};
use runvault::sync::{Compression, SyncReceipt};
use serde::Deserialize;

/// The specification for the flattened tables. Read, never transcribed.
const INDEX_COLUMNS: &str = include_str!("../../../schema/v1/index.columns.json");

/// Where the index is written, relative to the aggregation repository.
pub const INDEX_DIR: &str = "index";

#[derive(Debug, Deserialize)]
struct IndexSchema {
    tables: BTreeMap<String, TableSchema>,
}

#[derive(Debug, Deserialize)]
struct TableSchema {
    columns: Vec<ColumnSchema>,
}

#[derive(Debug, Deserialize)]
struct ColumnSchema {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

impl ColumnSchema {
    /// The DuckDB type for one of the five the specification uses.
    fn sql_type(&self) -> Result<&'static str, String> {
        Ok(match self.kind.as_str() {
            "string" => "VARCHAR",
            "bigint" => "BIGINT",
            "double" => "DOUBLE",
            "boolean" => "BOOLEAN",
            "timestamp" => "TIMESTAMP",
            other => return Err(format!("列 `{}` の型 `{other}` を知りません", self.name)),
        })
    }
}

/// One value in one column.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// Recorded as absent. Never a stand-in for a value nobody wrote down.
    Null,
    /// A string.
    Text(String),
    /// A whole number.
    Int(i64),
    /// A real number.
    Float(f64),
    /// A flag.
    Bool(bool),
}

impl Cell {
    fn text(value: Option<&str>) -> Self {
        value.map_or(Cell::Null, |v| Cell::Text(v.to_string()))
    }

    fn int(value: Option<i64>) -> Self {
        value
            .map_or(Cell::Int(0), Cell::Int)
            .clamp_null(value.is_none())
    }

    fn clamp_null(self, is_none: bool) -> Self {
        if is_none { Cell::Null } else { self }
    }

    fn float(value: Option<f64>) -> Self {
        value.map_or(Cell::Null, Cell::Float)
    }

    fn bool(value: Option<bool>) -> Self {
        value.map_or(Cell::Null, Cell::Bool)
    }
}

/// One row, addressed by column name so the writer decides the order.
pub type Row = BTreeMap<&'static str, Cell>;

/// The rows of every table, before they are written.
#[derive(Debug, Default)]
pub struct IndexRows {
    /// Table name to rows.
    pub tables: BTreeMap<&'static str, Vec<Row>>,
    /// What could not be read, so a missing run is never silent.
    pub notes: Vec<String>,
}

impl IndexRows {
    fn push(&mut self, table: &'static str, row: Row) {
        self.tables.entry(table).or_default().push(row);
    }
}

/// Reads the timestamp the record wrote and normalizes it to UTC.
///
/// The index compares runs made on machines in different places, so the column
/// holds one clock. The original offset stays in `run.json`, which is the record.
fn timestamp(value: Option<&str>) -> Result<Cell, String> {
    let Some(value) = value else {
        return Ok(Cell::Null);
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|e| format!("`{value}` を日時として読めません: {e}"))?;
    Ok(Cell::Text(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S%.6f")
            .to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Walking the aggregation repository
// ---------------------------------------------------------------------------

/// Every synced run directory under `vault_root`, by its receipt.
///
/// A run is a directory holding `sync.json`; nothing else is examined, so a
/// half-copied tree contributes nothing rather than contributing a partial run.
pub fn synced_runs(vault_root: &Path) -> std::io::Result<Vec<(PathBuf, SyncReceipt)>> {
    let mut out = Vec::new();
    let mut stack = vec![vault_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // The index lives inside the repository it describes.
        if dir.file_name().is_some_and(|n| n == INDEX_DIR) {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() || path.symlink_metadata()?.file_type().is_symlink() {
                continue;
            }
            let receipt = path.join(runvault::sync::RECEIPT);
            if receipt.is_file()
                && let Ok(parsed) = serde_json::from_str(&std::fs::read_to_string(&receipt)?)
            {
                out.push((path, parsed));
                continue;
            }
            stack.push(path);
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The bytes of one file the receipt says was stored, decompressing if needed.
fn stored_bytes(dir: &Path, receipt: &SyncReceipt, path: &str) -> Option<Vec<u8>> {
    let file = receipt.files.iter().find(|f| f.path == path)?;
    let stored = dir.join(&file.stored_path);
    let handle = std::fs::File::open(&stored).ok()?;
    match file.compression {
        Compression::None => std::io::read_to_string(handle).ok().map(String::into_bytes),
        Compression::Zstd => zstd::decode_all(handle).ok(),
    }
}

fn stored_json<T: serde::de::DeserializeOwned>(
    dir: &Path,
    receipt: &SyncReceipt,
    path: &str,
) -> Option<T> {
    serde_json::from_slice(&stored_bytes(dir, receipt, path)?).ok()
}

/// A CSV read into `(header, rows)`.
fn stored_csv(
    dir: &Path,
    receipt: &SyncReceipt,
    path: &str,
) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let bytes = stored_bytes(dir, receipt, path)?;
    let mut reader = csv_reader(&bytes);
    let header: Vec<String> = reader.headers().ok()?.iter().map(str::to_string).collect();
    let rows = reader
        .records()
        .filter_map(|r| r.ok())
        .map(|r| r.iter().map(str::to_string).collect())
        .collect();
    Some((header, rows))
}

fn csv_reader(bytes: &[u8]) -> csv::Reader<&[u8]> {
    csv::Reader::from_reader(bytes)
}

fn field<'a>(header: &[String], row: &'a [String], name: &str) -> Option<&'a str> {
    let value = header
        .iter()
        .position(|h| h == name)
        .and_then(|i| row.get(i))?;
    (!value.is_empty()).then_some(value.as_str())
}

// ---------------------------------------------------------------------------
// Flattening
// ---------------------------------------------------------------------------

/// Builds every row of every table from one aggregation repository.
pub fn collect(vault_root: &Path) -> Result<IndexRows, String> {
    let mut rows = IndexRows::default();
    let found = synced_runs(vault_root).map_err(|e| format!("{}: {e}", vault_root.display()))?;
    for (dir, receipt) in found {
        match receipt.run_uid {
            Some(_) => flatten_canonical(&dir, &receipt, &mut rows)?,
            None => flatten_legacy(&dir, &receipt, &mut rows)?,
        }
    }
    Ok(rows)
}

fn flatten_canonical(
    dir: &Path,
    receipt: &SyncReceipt,
    rows: &mut IndexRows,
) -> Result<(), String> {
    let Some(meta) = stored_json::<RunMeta>(dir, receipt, "run.json") else {
        rows.notes
            .push(format!("{}: run.json を読めませんでした", dir.display()));
        return Ok(());
    };
    let status: Option<RunStatus> = stored_json(dir, receipt, "status.json");
    let key = receipt.run_key.clone();

    let code = meta.code.as_ref();
    let rng = meta.rng.as_ref();
    let llm = meta.llm.as_ref();
    let lineage = meta.lineage.as_ref();
    let work = meta.research.work.as_ref();
    let counts = status.as_ref().and_then(|s| s.counts.as_ref());

    let mut run = Row::new();
    run.insert("run_key", Cell::Text(key.clone()));
    run.insert("run_uid", Cell::Text(meta.run_uid.clone()));
    run.insert("run_slug", Cell::Text(meta.run_slug.clone()));
    run.insert("schema_version", Cell::Text(meta.schema_version.clone()));
    run.insert("vocab_version", Cell::Text(meta.vocab_version.clone()));
    run.insert("repo_id", Cell::Text(meta.repo_id.clone()));
    run.insert("experiment", Cell::Text(meta.experiment.clone()));
    run.insert("subcommand", Cell::Text(meta.subcommand.clone()));
    run.insert("domain", Cell::Text(meta.domain.clone()));
    run.insert("config_hash", Cell::Text(meta.config_hash.clone()));
    run.insert("execution_hash", Cell::Text(meta.execution_hash.clone()));
    run.insert("env_hash", Cell::Text(meta.env.env_hash.clone()));
    run.insert("created_at", timestamp(Some(&meta.created_at))?);
    run.insert(
        "origin",
        Cell::Text(format!("{:?}", meta.origin).to_lowercase()),
    );
    run.insert(
        "visibility",
        Cell::Text(format!("{:?}", meta.visibility).to_lowercase()),
    );
    run.insert(
        "git_remote",
        Cell::text(code.and_then(|c| c.git_remote.as_deref())),
    );
    run.insert(
        "git_branch",
        Cell::text(code.and_then(|c| c.git_branch.as_deref())),
    );
    run.insert(
        "git_commit",
        Cell::text(code.map(|c| c.git_commit.as_str())),
    );
    run.insert("git_dirty", Cell::bool(code.map(|c| c.git_dirty)));
    run.insert(
        "dirty_hash",
        Cell::text(
            code.and_then(|c| c.dirty_hash.as_ref())
                .map(|h| h.value.as_str()),
        ),
    );
    run.insert(
        "repo_relpath",
        Cell::text(code.and_then(|c| c.repo_relpath.as_deref())),
    );
    run.insert(
        "master_seed",
        Cell::int(rng.and_then(|r| r.master_seed).map(|s| s as i64)),
    );
    run.insert(
        "replicate_index",
        Cell::int(rng.and_then(|r| r.replicate_index).map(|i| i as i64)),
    );
    run.insert("host", Cell::Text(meta.env.host.clone()));
    run.insert("os", Cell::Text(meta.env.os.clone()));
    run.insert("arch", Cell::Text(meta.env.arch.clone()));
    run.insert(
        "rustc_version",
        Cell::text(meta.env.rustc_version.as_deref()),
    );
    run.insert(
        "python_version",
        Cell::text(meta.env.python_version.as_deref()),
    );
    run.insert("llm_provider", Cell::text(llm.map(|l| l.provider.as_str())));
    run.insert(
        "llm_model_snapshot",
        Cell::text(llm.map(|l| l.model_snapshot.as_str())),
    );
    run.insert(
        "sweep_id",
        Cell::text(lineage.and_then(|l| l.sweep_id.as_deref())),
    );
    run.insert(
        "parent_run_uid",
        Cell::text(lineage.and_then(|l| l.parent_run_uid.as_deref())),
    );
    run.insert(
        "resumed_from",
        Cell::text(lineage.and_then(|l| l.resumed_from.as_deref())),
    );
    run.insert(
        "derived_from",
        Cell::text(lineage.and_then(|l| l.derived_from.as_deref())),
    );
    run.insert("is_replication", Cell::Bool(meta.research.is_replication));
    run.insert("work_id", Cell::text(work.map(|w| w.work_id.as_str())));
    run.insert("doi", Cell::text(work.and_then(|w| w.doi.as_deref())));
    run.insert(
        "arxiv_id",
        Cell::text(work.and_then(|w| w.arxiv_id.as_deref())),
    );
    run.insert(
        "paper_id",
        Cell::text(work.and_then(|w| w.paper_id.as_deref())),
    );
    run.insert("title", Cell::text(work.map(|w| w.title.as_str())));
    run.insert("year", Cell::int(work.and_then(|w| w.year)));
    run.insert(
        "source_version",
        Cell::text(work.and_then(|w| w.source_version.as_deref())),
    );
    run.insert(
        "obsidian_note",
        Cell::text(meta.research.obsidian_note.as_deref()),
    );
    // A run with no `status.json` did not finish and did not record a failure;
    // `unfinished` is that third state, not a guess about which of the two.
    run.insert(
        "state",
        Cell::Text(match status.as_ref().map(|s| s.state) {
            Some(State::Finished) => "finished".into(),
            Some(State::Failed) => "failed".into(),
            None => "unfinished".to_string(),
        }),
    );
    run.insert(
        "started_at",
        timestamp(status.as_ref().map(|s| s.started_at.as_str()))?,
    );
    run.insert(
        "finished_at",
        timestamp(status.as_ref().map(|s| s.finished_at.as_str()))?,
    );
    run.insert(
        "duration_sec",
        Cell::float(status.as_ref().map(|s| s.duration_sec)),
    );
    run.insert(
        "exit_code",
        Cell::int(status.as_ref().and_then(|s| s.exit_code)),
    );
    run.insert(
        "collision_index",
        Cell::int(
            status
                .as_ref()
                .and_then(|s| s.collision_index)
                .map(|i| i as i64),
        ),
    );
    run.insert(
        "error_kind",
        Cell::text(
            status
                .as_ref()
                .and_then(|s| s.error.as_ref())
                .map(|e| e.kind.as_str()),
        ),
    );
    run.insert("n_metrics", Cell::int(counts.map(|c| c.metrics as i64)));
    run.insert("n_events", Cell::int(counts.map(|c| c.events as i64)));
    run.insert("n_artifacts", Cell::int(counts.map(|c| c.artifacts as i64)));
    rows.push("runs", run);

    for dataset in &meta.data {
        let mut row = Row::new();
        row.insert("run_key", Cell::Text(key.clone()));
        row.insert("run_uid", Cell::Text(meta.run_uid.clone()));
        row.insert("role", Cell::Text(dataset.role.clone()));
        row.insert("name", Cell::Text(dataset.name.clone()));
        row.insert("dataset_id", Cell::text(dataset.dataset_id.as_deref()));
        row.insert("version", Cell::text(dataset.version.as_deref()));
        row.insert(
            "hash_algorithm",
            Cell::text(
                dataset
                    .hash
                    .as_ref()
                    .map(runvault::meta::Hash::algorithm_str),
            ),
        );
        row.insert(
            "hash_value",
            Cell::text(dataset.hash.as_ref().map(|h| h.value.as_str())),
        );
        let scope = dataset.hash_scope_str();
        row.insert(
            "hash_scope",
            Cell::text((!scope.is_empty()).then_some(scope)),
        );
        row.insert("n", Cell::int(dataset.n.map(|n| n as i64)));
        row.insert("uri", Cell::text(dataset.uri.as_deref()));
        row.insert("split", Cell::text(dataset.split.as_deref()));
        rows.push("run_data", row);
    }

    for target in &meta.research.targets {
        let mut row = Row::new();
        row.insert("run_key", Cell::Text(key.clone()));
        row.insert("run_uid", Cell::Text(meta.run_uid.clone()));
        row.insert("target_id", Cell::Text(target.target_id.clone()));
        row.insert(
            "kind",
            Cell::Text(format!("{:?}", target.kind).to_lowercase()),
        );
        row.insert("label", Cell::Text(target.label.clone()));
        row.insert("panel", Cell::text(target.panel.as_deref()));
        row.insert("row", Cell::text(target.row.as_deref()));
        row.insert("condition", Cell::text(target.condition.as_deref()));
        rows.push("run_targets", row);
    }

    for issue in &meta.research.jira {
        let mut row = Row::new();
        row.insert("run_key", Cell::Text(key.clone()));
        row.insert("run_uid", Cell::Text(meta.run_uid.clone()));
        row.insert("issue_key", Cell::Text(issue.clone()));
        rows.push("run_jira", row);
    }

    if let Some((header, csv)) = stored_csv(dir, receipt, "metrics.csv") {
        for record in &csv {
            let mut row = Row::new();
            row.insert("run_key", Cell::Text(key.clone()));
            row.insert("run_uid", Cell::Text(meta.run_uid.clone()));
            row.insert(
                "step",
                Cell::int(field(&header, record, "step").and_then(|v| v.parse().ok())),
            );
            row.insert("step_unit", Cell::text(field(&header, record, "step_unit")));
            row.insert("scope", Cell::text(field(&header, record, "scope")));
            row.insert("name", Cell::text(field(&header, record, "name")));
            row.insert(
                "value",
                Cell::float(field(&header, record, "value").and_then(|v| v.parse().ok())),
            );
            rows.push("metrics", row);
        }
    }

    if let Some((header, csv)) = stored_csv(dir, receipt, "reference.csv") {
        for record in &csv {
            let mut row = Row::new();
            row.insert("run_key", Cell::Text(key.clone()));
            row.insert("run_uid", Cell::Text(meta.run_uid.clone()));
            row.insert(
                "step",
                Cell::int(field(&header, record, "step").and_then(|v| v.parse().ok())),
            );
            row.insert("step_unit", Cell::text(field(&header, record, "step_unit")));
            row.insert("scope", Cell::text(field(&header, record, "scope")));
            row.insert("name", Cell::text(field(&header, record, "name")));
            row.insert(
                "value",
                Cell::float(field(&header, record, "value").and_then(|v| v.parse().ok())),
            );
            row.insert("target_id", Cell::text(field(&header, record, "target_id")));
            row.insert("source", Cell::text(field(&header, record, "source")));
            rows.push("reference", row);
        }
    }

    if let Some((header, csv)) = stored_csv(dir, receipt, "manifest.csv") {
        for record in &csv {
            let mut row = Row::new();
            row.insert("run_key", Cell::Text(key.clone()));
            row.insert("run_uid", Cell::Text(meta.run_uid.clone()));
            row.insert("path", Cell::text(field(&header, record, "path")));
            row.insert("algorithm", Cell::text(field(&header, record, "algorithm")));
            row.insert("digest", Cell::text(field(&header, record, "digest")));
            row.insert(
                "bytes",
                Cell::int(field(&header, record, "bytes").and_then(|v| v.parse().ok())),
            );
            rows.push("manifest", row);
        }
    }
    Ok(())
}

/// Flattens a legacy run, which has no `run.json` and no `run_uid`.
///
/// Nothing is invented for the columns it cannot fill: they stay `NULL`, and a
/// query that wants only the runs written against this specification asks for
/// `run_uid IS NOT NULL`.
fn flatten_legacy(dir: &Path, receipt: &SyncReceipt, rows: &mut IndexRows) -> Result<(), String> {
    let read = read_legacy(dir, receipt)?;
    let key = receipt.run_key.clone();
    rows.notes
        .extend(read.notes.iter().map(|n| format!("{}: {n}", dir.display())));

    let mut run = Row::new();
    for column in ALL_RUN_COLUMNS_NULL_FOR_LEGACY {
        run.insert(column, Cell::Null);
    }
    run.insert("run_key", Cell::Text(key.clone()));
    run.insert("run_uid", Cell::Null);
    run.insert("repo_id", Cell::Text(receipt.source.repo_id.clone()));
    run.insert("experiment", Cell::text(read.experiment.as_deref()));
    run.insert("subcommand", Cell::text(read.subcommand.as_deref()));
    run.insert("is_replication", Cell::Null);
    run.insert("state", Cell::Null);
    run.insert("created_at", legacy_timestamp(read.timestamp.as_deref())?);
    rows.push("runs", run);

    for metric in &read.metrics {
        let mut row = Row::new();
        row.insert("run_key", Cell::Text(key.clone()));
        row.insert("run_uid", Cell::Null);
        row.insert("step", Cell::int(metric.step));
        row.insert("step_unit", Cell::text(metric.step_unit.as_deref()));
        row.insert("scope", Cell::Text(metric.scope.clone()));
        row.insert("name", Cell::Text(metric.name.clone()));
        row.insert("value", Cell::Float(metric.value));
        rows.push("metrics", row);
    }
    Ok(())
}

/// A legacy directory name carries `YYYYMMDD_HHMMSS` and no time zone.
///
/// It is read as local time, which is where it was written, and then held to
/// the same UTC column as everything else.
fn legacy_timestamp(value: Option<&str>) -> Result<Cell, String> {
    let Some(value) = value else {
        return Ok(Cell::Null);
    };
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d_%H%M%S")
        .map_err(|e| format!("`{value}` を日時として読めません: {e}"))?;
    let local = naive
        .and_local_timezone(chrono::Local)
        .earliest()
        .ok_or_else(|| format!("`{value}` は現地時刻として存在しません"))?;
    Ok(Cell::Text(
        local
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S%.6f")
            .to_string(),
    ))
}

/// Reads a legacy run out of the aggregation repository.
///
/// Anything stored compressed is expanded into a temporary directory first: the
/// reader works on files, and a `.zst` it cannot open would look like a run
/// that simply had no metrics.
fn read_legacy(dir: &Path, receipt: &SyncReceipt) -> Result<LegacyRun, String> {
    let compressed = receipt
        .files
        .iter()
        .any(|f| f.compression != Compression::None);
    let repo_id = &receipt.source.repo_id;

    if !compressed {
        // `<vault>/<repo_id>` plays the part `results/` played at the source,
        // because that is the shape `sync` preserved.
        let root = results_root_of(dir, receipt);
        return legacy::read_run(&root, repo_id, dir)
            .map_err(|e| format!("{}: {e}", dir.display()));
    }

    let temporary = tempfile::tempdir().map_err(|e| format!("{}: {e}", dir.display()))?;
    let run_dir = temporary.path().join(dir.file_name().unwrap_or_default());
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    for file in &receipt.files {
        let Some(bytes) = stored_bytes(dir, receipt, &file.path) else {
            continue;
        };
        let target = run_dir.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    legacy::read_run(temporary.path(), repo_id, &run_dir)
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// The directory that plays the part of the source `results/` for a legacy run.
fn results_root_of(dir: &Path, receipt: &SyncReceipt) -> PathBuf {
    // The key is `legacy:<repo_id>:<relative path>`; the relative path has as
    // many components as must be trimmed from the destination to get back to
    // the root the key was measured from.
    let depth = receipt
        .run_key
        .rsplit(':')
        .next()
        .map(|rel| rel.split('/').count())
        .unwrap_or(1);
    let mut root = dir.to_path_buf();
    for _ in 0..depth {
        root = root.parent().map(Path::to_path_buf).unwrap_or(root);
    }
    root
}

/// Columns a legacy run leaves empty, listed so none is forgotten.
const ALL_RUN_COLUMNS_NULL_FOR_LEGACY: [&str; 45] = [
    "run_slug",
    "schema_version",
    "vocab_version",
    "domain",
    "config_hash",
    "execution_hash",
    "env_hash",
    "origin",
    "visibility",
    "git_remote",
    "git_branch",
    "git_commit",
    "git_dirty",
    "dirty_hash",
    "repo_relpath",
    "master_seed",
    "replicate_index",
    "host",
    "os",
    "arch",
    "rustc_version",
    "python_version",
    "llm_provider",
    "llm_model_snapshot",
    "sweep_id",
    "parent_run_uid",
    "resumed_from",
    "derived_from",
    "work_id",
    "doi",
    "arxiv_id",
    "paper_id",
    "title",
    "year",
    "source_version",
    "obsidian_note",
    "started_at",
    "finished_at",
    "duration_sec",
    "exit_code",
    "collision_index",
    "error_kind",
    "n_metrics",
    "n_events",
    "n_artifacts",
];

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// What one refresh produced.
#[derive(Debug, Default)]
pub struct Refreshed {
    /// Rows written, per table.
    pub counts: BTreeMap<String, usize>,
    /// What could not be read.
    pub notes: Vec<String>,
}

/// Rebuilds `index/*.parquet` from the aggregation repository.
///
/// The whole index is rewritten rather than amended. It is derived from the
/// receipts, and a partial update would leave a table describing runs that are
/// no longer there while claiming to describe the repository.
pub fn refresh(vault_root: &Path) -> Result<Refreshed, String> {
    let schema: IndexSchema = serde_json::from_str(INDEX_COLUMNS)
        .map_err(|e| format!("index.columns.json を読めません: {e}"))?;
    let rows = collect(vault_root)?;

    let index_dir = vault_root.join(INDEX_DIR);
    std::fs::create_dir_all(&index_dir).map_err(|e| format!("{}: {e}", index_dir.display()))?;
    // The index is derived from the receipts and can always be rebuilt, so it
    // does not belong in the history of the repository it describes. The
    // directory excludes itself rather than editing the repository's own
    // `.gitignore`, which is not this command's to change.
    std::fs::write(index_dir.join(".gitignore"), "*\n")
        .map_err(|e| format!("{}: {e}", index_dir.display()))?;

    let connection = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
    let mut counts = BTreeMap::new();

    for (name, table) in &schema.tables {
        let columns = table
            .columns
            .iter()
            .map(|c| Ok(format!("{} {}", quote(&c.name), c.sql_type()?)))
            .collect::<Result<Vec<_>, String>>()?;
        connection
            .execute_batch(&format!(
                "CREATE TABLE {} ({});",
                quote(name),
                columns.join(", ")
            ))
            .map_err(|e| format!("{name}: {e}"))?;

        let empty = Vec::new();
        let table_rows = rows.tables.get(name.as_str()).unwrap_or(&empty);
        insert_rows(&connection, name, table, table_rows)?;

        let path = index_dir.join(format!("{name}.parquet"));
        connection
            .execute_batch(&format!(
                "COPY {} TO '{}' (FORMAT PARQUET);",
                quote(name),
                path.display()
            ))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        counts.insert(name.clone(), table_rows.len());
    }

    Ok(Refreshed {
        counts,
        notes: rows.notes,
    })
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Inserts the rows in the order the specification declares the columns.
///
/// A row that has no value for a declared column is an error rather than a
/// `NULL`: the two are different, and only one of them means "recorded absent".
fn insert_rows(
    connection: &duckdb::Connection,
    name: &str,
    table: &TableSchema,
    rows: &[Row],
) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = table.columns.iter().map(|c| quote(&c.name)).collect();
    let placeholders: Vec<&str> = table.columns.iter().map(|_| "?").collect();
    let statement = format!(
        "INSERT INTO {} ({}) VALUES ({});",
        quote(name),
        names.join(", "),
        placeholders.join(", ")
    );
    let mut prepared = connection
        .prepare(&statement)
        .map_err(|e| format!("{name}: {e}"))?;

    for (i, row) in rows.iter().enumerate() {
        let mut values: Vec<duckdb::types::Value> = Vec::with_capacity(table.columns.len());
        for column in &table.columns {
            let cell = row.get(column.name.as_str()).ok_or_else(|| {
                format!("{name} の {i} 行目に列 `{}` の値がありません", column.name)
            })?;
            values.push(match cell {
                Cell::Null => duckdb::types::Value::Null,
                Cell::Text(text) => duckdb::types::Value::Text(text.clone()),
                Cell::Int(number) => duckdb::types::Value::BigInt(*number),
                Cell::Float(number) => duckdb::types::Value::Double(*number),
                Cell::Bool(flag) => duckdb::types::Value::Boolean(*flag),
            });
        }
        prepared
            .execute(duckdb::params_from_iter(values))
            .map_err(|e| format!("{name} の {i} 行目: {e}"))?;
    }
    Ok(())
}
