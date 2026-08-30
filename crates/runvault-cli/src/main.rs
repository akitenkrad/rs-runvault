//! The `runvault` command line.
//!
//! The subcommands that operate on a single machine's run directories:
//! finding one, checking one, and cleaning up after a killed one. `sync`,
//! `query` and `report` arrive with the aggregation layer.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod index;
mod report;

use clap::{Args, Parser, Subcommand};
use runvault::gc::Outcome;
use runvault::meta::RunMeta;
use runvault::{Result, files, paths, sync, verify};

#[derive(Parser)]
#[command(name = "runvault", version, about = "Plain-file experiment tracking")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print run directories: the latest finished one, or the ones sharing a condition.
    Path(PathArgs),
    /// Check a run directory against the invariants that span its files.
    Verify(VerifyArgs),
    /// Turn runs whose process was killed into recorded failures.
    Gc(GcArgs),
    /// Read run directories written before this specification existed.
    Legacy(LegacyArgs),
    /// Copy the light half of every run to the aggregation repository.
    Sync(SyncArgs),
    /// Rebuild the index, run SQL against it, or both.
    Query(QueryArgs),
    /// Summarize the index for the Obsidian dashboard.
    Report(ReportArgs),
}

#[derive(Args)]
struct PathArgs {
    /// The experiment to look in.
    #[arg(long)]
    experiment: String,
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// Resolve the `latest_finished` link.
    #[arg(long, conflicts_with_all = ["config_hash", "execution_hash"])]
    latest: bool,
    /// Print every run whose `config_hash` starts with this prefix (the same condition).
    #[arg(long)]
    config_hash: Option<String>,
    /// Print every run whose `execution_hash` starts with this prefix.
    ///
    /// This is what answers "has this exact thing already been run": the same
    /// condition, the same seeds, the same commit and the same environment.
    #[arg(long)]
    execution_hash: Option<String>,
    /// Only consider runs that finished. A failed run is not a run that happened.
    #[arg(long)]
    finished: bool,
    /// Only consider runs of this subcommand.
    ///
    /// A sweep's parent and its children share an experiment, so `--latest`
    /// alone can hand back the parent, which holds no metrics of its own.
    #[arg(long)]
    subcommand: Option<String>,
    /// Only consider runs that belong to no sweep.
    ///
    /// A sweep's children have the same subcommand as a run started by hand, so
    /// narrowing by subcommand alone still hands back the last child.
    #[arg(long, conflicts_with = "children_of")]
    standalone: bool,
    /// Only consider the children of this sweep parent, by its `run_uid`.
    #[arg(long, value_name = "RUN_UID")]
    children_of: Option<String>,
}

#[derive(Args)]
struct VerifyArgs {
    /// The run directory.
    run: PathBuf,
    /// Also recompute the hashes, rehash the artifacts and walk `events.jsonl`.
    ///
    /// The cost scales with the size of the run, which is why it is not what
    /// every execution does on its way out. `sync` runs it before it copies.
    #[arg(long)]
    deep: bool,
}

#[derive(Args)]
struct LegacyArgs {
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// The stable repository id the keys are built from.
    #[arg(long)]
    repo_id: String,
    /// Print the runs as JSON instead of a summary.
    #[arg(long)]
    json: bool,
    /// Also print what each run could not convert.
    #[arg(long)]
    notes: bool,
}

#[derive(Args)]
struct SyncArgs {
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// The stable repository id. Legacy keys are built from it, and a canonical
    /// run whose `run.json` disagrees with it is an error rather than a guess.
    #[arg(long)]
    repo_id: String,
    /// The aggregation repository. It must declare itself private.
    #[arg(long)]
    vault: Option<PathBuf>,
    /// List what would be copied, and how large it is, without writing.
    #[arg(long)]
    dry_run: bool,
    /// Also send runs that did not declare themselves public.
    #[arg(long)]
    allow_internal: bool,
}

#[derive(Args)]
struct QueryArgs {
    /// The SQL to run. The table files are `index/<name>.parquet`.
    sql: Option<String>,
    /// The aggregation repository. The index is written inside it.
    #[arg(long)]
    vault: Option<PathBuf>,
    /// Walk the repository and rebuild `index/*.parquet` first.
    #[arg(long)]
    refresh: bool,
}

#[derive(Args)]
struct ReportArgs {
    /// The aggregation repository whose index is summarized.
    #[arg(long)]
    vault: Option<PathBuf>,
    /// Write the payload the Obsidian dashboard reads.
    #[arg(long)]
    obsidian: bool,
    /// Where to write it. Defaults to standard output.
    #[arg(long, short)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct GcArgs {
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// Report what would happen without writing anything.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Path(args) => cmd_path(&args),
        Command::Verify(args) => cmd_verify(&args),
        Command::Gc(args) => cmd_gc(&args),
        Command::Legacy(args) => cmd_legacy(&args),
        Command::Sync(args) => cmd_sync(&args),
        Command::Query(args) => cmd_query(&args),
        Command::Report(args) => cmd_report(&args),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("runvault: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_path(args: &PathArgs) -> Result<ExitCode> {
    let experiment_dir = paths::experiment_dir(&args.results_root, &args.experiment);

    let selected = select_runs(&experiment_dir, args)?;

    // `latest_finished` is one link per experiment, so narrowing by subcommand
    // means picking the newest finished run instead of following it.
    if args.latest {
        if args.subcommand.is_none() && !args.standalone && args.children_of.is_none() {
            let link = experiment_dir.join(paths::LATEST_FINISHED);
            if !link.exists() {
                eprintln!("runvault: {} がありません", link.display());
                return Ok(ExitCode::FAILURE);
            }
            println!("{}", std::fs::canonicalize(&link)?.display());
            return Ok(ExitCode::SUCCESS);
        }
        let newest = selected
            .into_iter()
            .filter_map(|dir| finished_at(&dir).map(|at| (at, dir)))
            .max_by(|a, b| a.0.cmp(&b.0));
        return Ok(match newest {
            Some((_, dir)) => {
                println!("{}", resolved(&dir).display());
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("runvault: 条件に合う完了済みの run がありません");
                ExitCode::FAILURE
            }
        });
    }

    let filtering = args.config_hash.is_some() || args.execution_hash.is_some();
    for dir in &selected {
        // Every path this command prints has the same shape, so a caller can
        // compare a parent against its children without normalizing first.
        println!("{}", resolved(dir).display());
    }
    // Nothing found is a failing exit code so a shell script can branch on it
    // without parsing the output. Listing everything is not a search, so an empty
    // experiment is not a failure there.
    Ok(if selected.is_empty() && filtering {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The runs of an experiment that pass every filter the caller gave.
fn select_runs(experiment_dir: &Path, args: &PathArgs) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in paths::run_dirs(experiment_dir)? {
        let Ok(meta) = files::read_json::<RunMeta>(&dir.join("run.json")) else {
            continue;
        };
        let matches = |prefix: &Option<String>, hash: &str| {
            prefix.as_ref().is_none_or(|p| hash.starts_with(p))
        };
        if !matches(&args.config_hash, &meta.config_hash)
            || !matches(&args.execution_hash, &meta.execution_hash)
        {
            continue;
        }
        if args
            .subcommand
            .as_ref()
            .is_some_and(|want| meta.subcommand != *want)
        {
            continue;
        }
        let parent = meta
            .lineage
            .as_ref()
            .and_then(|l| l.parent_run_uid.as_deref());
        if args.standalone && parent.is_some() {
            continue;
        }
        if let Some(wanted) = &args.children_of
            && parent != Some(wanted.as_str())
        {
            continue;
        }
        if (args.finished || args.latest) && !is_finished(&dir) {
            continue;
        }
        out.push(dir);
    }
    Ok(out)
}

/// When a run finished, for a run that did.
fn finished_at(dir: &Path) -> Option<String> {
    files::read_json::<runvault::RunStatus>(&dir.join("status.json"))
        .ok()
        .filter(|status| status.state == runvault::State::Finished)
        .map(|status| status.finished_at)
}

/// The path as the filesystem sees it, or as given when it cannot be resolved.
fn resolved(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Whether a run directory holds a `status.json` that says it finished.
fn is_finished(dir: &Path) -> bool {
    files::read_json::<runvault::RunStatus>(&dir.join("status.json"))
        .is_ok_and(|status| status.state == runvault::State::Finished)
}

fn cmd_verify(args: &VerifyArgs) -> Result<ExitCode> {
    let checked = if args.deep {
        verify::deep(&args.run)
    } else {
        verify::shallow(&args.run)
    };
    match checked {
        Ok(()) => {
            let depth = if args.deep { "ok (deep)" } else { "ok" };
            println!("{depth} {}", args.run.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{}: {e}", args.run.display());
            Ok(ExitCode::FAILURE)
        }
    }
}

fn cmd_gc(args: &GcArgs) -> Result<ExitCode> {
    let reaped = runvault::gc::collect(&args.results_root, args.dry_run)?;
    let mut stale = 0;
    for entry in &reaped {
        let label = match entry.outcome {
            Outcome::Running => "running",
            Outcome::Contested => "running (一度 stale と判定したが生きていた)",
            Outcome::Reaped => {
                stale += 1;
                if args.dry_run {
                    "stale (dry-run)"
                } else {
                    "reaped"
                }
            }
        };
        println!("{label}\t{}", relative_to_cwd(&entry.dir).display());
    }
    println!("{} 件を確認し，{stale} 件が異常終了でした", reaped.len());
    Ok(ExitCode::SUCCESS)
}

fn cmd_legacy(args: &LegacyArgs) -> Result<ExitCode> {
    let runs = runvault::legacy::read_all(&args.results_root, &args.repo_id)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(ExitCode::SUCCESS);
    }

    // The index keys on (run_key) and (run_key, name, step, step_unit, scope);
    // if those are not unique the runs cannot be joined, so it is checked here
    // rather than discovered when the index is built.
    let mut keys = std::collections::HashSet::new();
    let mut metric_keys = std::collections::HashSet::new();
    let mut collisions = 0;
    let (mut metrics, mut tables) = (0, 0);

    for run in &runs {
        if !keys.insert(run.run_key.clone()) {
            eprintln!("run_key が重複しています: {}", run.run_key);
            collisions += 1;
        }
        for metric in &run.metrics {
            let key = (
                run.run_key.clone(),
                metric.name.clone(),
                metric.step,
                metric.step_unit.clone(),
                metric.scope.clone(),
            );
            if !metric_keys.insert(key) {
                eprintln!(
                    "指標の主キーが重複しています: {} {}",
                    run.run_key, metric.name
                );
                collisions += 1;
            }
        }
        metrics += run.metrics.len();
        tables += run.tables.len();

        println!(
            "{}\t{} metrics\t{} tables\t{}",
            run.timestamp.as_deref().unwrap_or("-"),
            run.metrics.len(),
            run.tables.len(),
            run.relpath
        );
        if args.notes {
            for note in &run.notes {
                println!("  note: {note}");
            }
            for table in &run.tables {
                println!(
                    "  table: {} ({} 行) — {}",
                    table.file, table.rows, table.reason
                );
            }
        }
    }

    println!(
        "\nrun {} 件・指標 {metrics} 行・変換しなかった表 {tables} 件",
        runs.len()
    );
    if collisions > 0 {
        eprintln!("主キーの重複 {collisions} 件");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Where runs are aggregated when the caller names no other place.
fn default_vault() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("research").join("runs")
}

fn cmd_sync(args: &SyncArgs) -> Result<ExitCode> {
    let vault = args.vault.clone().unwrap_or_else(default_vault);
    // Reading the declaration first means a destination that never said it was
    // private stops the command before it has looked at a single run.
    let (declared_at, config) = sync::load_vault_config(&vault)?;
    println!(
        "集約先 {} ({} が宣言，{} MiB 超は zstd)",
        vault.display(),
        declared_at.join(sync::VAULT_CONFIG).display(),
        config.compress_over_mib
    );

    let options = sync::SyncOptions {
        allow_internal: args.allow_internal || config.allow_internal,
        compress_over_bytes: config.compress_over_bytes(),
    };
    let planned = sync::plan_all(&args.results_root, &args.repo_id, &vault, &options)?;

    let (mut sent, mut bytes, mut unverified) = (0u64, 0u64, 0u64);
    for entry in &planned {
        match entry {
            sync::Planned::Skipped { run_dir, reason } => {
                if reason.starts_with("verify") {
                    unverified += 1;
                }
                println!("skip\t{}\t{reason}", relative_to_cwd(run_dir).display());
            }
            sync::Planned::Send(plan) => {
                sent += 1;
                bytes += plan.bytes();
                println!(
                    "{}\t{} → {}\t{} ファイル・{} バイト",
                    if args.dry_run { "would send" } else { "send" },
                    relative_to_cwd(&plan.run_dir).display(),
                    plan.dest.display(),
                    plan.files.len(),
                    plan.bytes()
                );
                // What enters a git history is worth seeing before it does.
                for file in &plan.files {
                    println!(
                        "  {}\t{}\t{} バイト",
                        file.stored_path,
                        match file.compression {
                            sync::Compression::None => "そのまま",
                            sync::Compression::Zstd => "zstd",
                        },
                        file.bytes
                    );
                }
                if !args.dry_run {
                    let synced = sync::execute(plan)?;
                    println!("  受領証 generation {}", synced.receipt.generation);
                    // Not deleted: the aggregation copy may be the only one left.
                    for path in &synced.left_behind {
                        println!("  残置\t{path} (前回は送ったが今回は送っていません)");
                    }
                }
            }
        }
    }

    println!(
        "\n{sent} run・{bytes} バイトを{}",
        if args.dry_run {
            "同期します (--dry-run なので書いていません)"
        } else {
            "同期しました"
        }
    );
    // A run held back for being internal is a decision; one held back for
    // contradicting itself is a problem, and the exit code says so.
    Ok(if unverified > 0 {
        eprintln!("{unverified} 件が verify --deep に通らず送られていません");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_report(args: &ReportArgs) -> Result<ExitCode> {
    if !args.obsidian {
        eprintln!("runvault: いまのところ --obsidian だけが出力先です");
        return Ok(ExitCode::FAILURE);
    }
    let vault = args.vault.clone().unwrap_or_else(default_vault);
    let payload = report::build(&vault).map_err(runvault::Error::Spec)?;
    let text = serde_json::to_string_pretty(&payload)?;

    match &args.out {
        // Written whole or not at all: the dashboard reads this file on a timer,
        // and half of it parses as neither missing nor valid.
        Some(path) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            runvault::files::write_atomically(path, text.as_bytes())?;
            eprintln!(
                "{} 実験・{} run・{} 件の警告を {} に書きました",
                payload["experiments"].as_array().map_or(0, Vec::len),
                payload["runs"].as_array().map_or(0, Vec::len),
                payload["warnings"].as_array().map_or(0, Vec::len),
                path.display()
            );
        }
        None => println!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_query(args: &QueryArgs) -> Result<ExitCode> {
    let vault = args.vault.clone().unwrap_or_else(default_vault);
    if !args.refresh && args.sql.is_none() {
        eprintln!("runvault: --refresh か SQL のどちらかが要ります");
        return Ok(ExitCode::FAILURE);
    }

    if args.refresh {
        let refreshed = index::refresh(&vault).map_err(runvault::Error::Spec)?;
        for (table, rows) in &refreshed.counts {
            println!("{table}\t{rows} 行");
        }
        // A run the walk could not read is reported rather than left out
        // quietly: an index that is short by one run looks exactly like one
        // that is complete.
        for note in &refreshed.notes {
            eprintln!("note: {note}");
        }
    }

    let Some(sql) = &args.sql else {
        return Ok(ExitCode::SUCCESS);
    };

    // The documented queries name the tables as `index/runs.parquet`, so the
    // relative paths have to resolve against the repository, not the shell.
    std::env::set_current_dir(&vault)?;
    let connection = duckdb::Connection::open_in_memory().map_err(to_error)?;
    let mut statement = connection.prepare(sql).map_err(to_error)?;
    let mut rows = statement.query([]).map_err(to_error)?;

    let mut printed_header = false;
    let mut count = 0usize;
    while let Some(row) = rows.next().map_err(to_error)? {
        let statement = row.as_ref();
        if !printed_header {
            println!("{}", statement.column_names().join("\t"));
            printed_header = true;
        }
        let cells: Vec<String> = (0..statement.column_count())
            .map(|i| {
                row.get::<usize, duckdb::types::Value>(i)
                    .map(render)
                    .unwrap_or_default()
            })
            .collect();
        println!("{}", cells.join("\t"));
        count += 1;
    }
    eprintln!("{count} 行");
    Ok(ExitCode::SUCCESS)
}

/// A value as a column of text, with `NULL` spelled out rather than blank.
///
/// Every scalar the index can hold is named. Falling back to the debug format
/// would print `BigInt(2)` where a count belongs, which is the kind of output
/// that quietly ends up pasted into a table.
fn render(value: duckdb::types::Value) -> String {
    use duckdb::types::Value as V;
    match value {
        V::Null => "NULL".into(),
        V::Boolean(v) => v.to_string(),
        V::Text(v) => v,
        V::TinyInt(v) => v.to_string(),
        V::SmallInt(v) => v.to_string(),
        V::Int(v) => v.to_string(),
        V::BigInt(v) => v.to_string(),
        V::HugeInt(v) => v.to_string(),
        V::UTinyInt(v) => v.to_string(),
        V::USmallInt(v) => v.to_string(),
        V::UInt(v) => v.to_string(),
        V::UBigInt(v) => v.to_string(),
        V::Float(v) => v.to_string(),
        V::Double(v) => v.to_string(),
        V::Decimal(v) => v.to_string(),
        V::Timestamp(unit, count) => render_timestamp(unit, count),
        other => format!("{other:?}"),
    }
}

/// A DuckDB timestamp as the UTC instant the index stored.
fn render_timestamp(unit: duckdb::types::TimeUnit, count: i64) -> String {
    use duckdb::types::TimeUnit;
    let micros = match unit {
        TimeUnit::Second => count.saturating_mul(1_000_000),
        TimeUnit::Millisecond => count.saturating_mul(1_000),
        TimeUnit::Microsecond => count,
        TimeUnit::Nanosecond => count / 1_000,
    };
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|at| at.format("%Y-%m-%d %H:%M:%S%.6f").to_string())
        .unwrap_or_else(|| count.to_string())
}

fn to_error(e: duckdb::Error) -> runvault::Error {
    runvault::Error::Spec(e.to_string())
}

fn relative_to_cwd(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
}
