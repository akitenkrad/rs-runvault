//! Reading run directories that were written before this specification existed.
//!
//! These runs have no `run.json`, no identifier and no status, and nothing here
//! invents one: a legacy run is carried into the index as `schema_version:
//! null`, keyed by its path, with every unknown field left empty. Writing a
//! `run.json` back into one would make it indistinguishable from a run that was
//! actually recorded to specification (design note §3.1 and §4.1).
//!
//! There is deliberately no writer for these shapes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::files;
use crate::ids;

/// Legacy column names that name a time axis, and the `step_unit` they map to.
///
/// A name that is not in this table is not an axis, however early it appears in
/// the header: real files start with `experiment`, `seed` or `respondent_id`,
/// none of which count steps.
const TIME_AXIS: [(&str, &str); 10] = [
    ("step", "step"),
    ("t", "step"),
    ("iteration", "step"),
    ("round", "round"),
    ("epoch", "epoch"),
    ("turn", "turn"),
    ("generation", "generation"),
    ("day", "day"),
    ("month", "month"),
    ("year", "year"),
];

/// `step_unit` values that this maps to but `vocabulary.toml` does not register.
const UNREGISTERED_UNITS: [&str; 3] = ["day", "month", "year"];

/// Column names that hold a metric's name in an already-long legacy file.
const LONG_NAME_COLUMNS: [&str; 2] = ["name", "metric"];

/// Files whose columns may be read as metrics.
///
/// Every other CSV in a run is a panel, a snapshot or a table of its own; giving
/// them the same treatment would turn per-agent rows into run-level numbers.
fn is_metric_file(stem: &str) -> bool {
    stem.starts_with("metrics") || stem.ends_with("_metrics") || stem.ends_with("_summary")
}

/// The parts a legacy directory name carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyName {
    /// `YYYYMMDD_HHMMSS`, as written.
    pub timestamp: String,
    /// The part before the timestamp (`reproduce_20260530_220555`).
    pub prefix: Option<String>,
    /// The part after it (`20260620_162729_sweep`).
    pub suffix: Option<String>,
}

/// Parses a legacy run directory name, or `None` when it is not one.
///
/// All four shapes the review-kit rule set already permits are accepted:
/// `<ts>`, `<prefix>_<ts>`, `<ts>_<suffix>` and `<prefix>_<ts>_<suffix>`.
pub fn parse_dir_name(name: &str) -> Option<LegacyName> {
    let bytes = name.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';

    for start in 0..bytes.len().saturating_sub(14) {
        if start > 0 && bytes[start - 1] != b'_' {
            continue;
        }
        let end = start + 15;
        if end > bytes.len() {
            break;
        }
        if end < bytes.len() && bytes[end] != b'_' {
            continue;
        }
        let candidate = &name[start..end];
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !(digits(&candidate[..8]) && candidate.as_bytes()[8] == b'_' && digits(&candidate[9..]))
        {
            continue;
        }

        let prefix = (start > 0).then(|| name[..start - 1].to_string());
        let suffix = (end < bytes.len()).then(|| name[end + 1..].to_string());
        if !name.bytes().all(is_word) {
            return None;
        }
        return Some(LegacyName {
            timestamp: candidate.to_string(),
            prefix,
            suffix,
        });
    }
    None
}

/// One number read out of a legacy file, in the long shape the index uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegacyMetric {
    /// Position on the time axis, or `None` for an aggregate.
    pub step: Option<i64>,
    /// What `step` counts.
    pub step_unit: Option<String>,
    /// How coarse the value is. Always `run` for a legacy file.
    pub scope: String,
    /// The metric name.
    pub name: String,
    /// The value.
    pub value: f64,
    /// The file it came from, relative to the run directory.
    pub source: String,
}

/// A table that was left as a table, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegacyTable {
    /// The file, relative to the run directory.
    pub file: String,
    /// Its header, as written.
    pub header: Vec<String>,
    /// How many data rows it has.
    pub rows: usize,
    /// Why it did not become metrics.
    pub reason: String,
}

/// A run that predates the specification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegacyRun {
    /// `legacy:<repo_id>:<path from the results root>`. The index's primary key.
    pub run_key: String,
    /// The repository, supplied by the caller.
    pub repo_id: String,
    /// The run directory relative to the results root, normalized.
    pub relpath: String,
    /// Where the run actually is.
    pub dir: PathBuf,
    /// The grouping directories above it, joined with `/`, when there are any.
    pub experiment: Option<String>,
    /// The prefix or suffix of the directory name, when it has one.
    pub subcommand: Option<String>,
    /// The timestamp in the name, as written. Not a schema `date-time`.
    pub timestamp: Option<String>,
    /// `config.json` as it stands: a flat parameter object, not an envelope.
    pub parameters: Option<Value>,
    /// Numbers that could be read without guessing.
    pub metrics: Vec<LegacyMetric>,
    /// Tables that were not converted.
    pub tables: Vec<LegacyTable>,
    /// Every other file in the run directory, relative to it.
    pub extras: Vec<String>,
    /// What was skipped, and why. Never silent.
    pub notes: Vec<String>,
}

/// Percent-encodes the two characters that would otherwise make `run_key` ambiguous.
///
/// `%` goes first: encoding `:` first would then have its own `%` re-encoded.
fn encode_key_part(part: &str) -> String {
    part.replace('%', "%25").replace(':', "%3A")
}

/// The index key for a legacy run.
pub fn run_key(repo_id: &str, relpath: &str) -> String {
    format!(
        "legacy:{}:{}",
        encode_key_part(repo_id),
        encode_key_part(relpath)
    )
}

/// Normalizes a path relative to the results root into the string both
/// implementations must produce.
///
/// `.` elements are dropped and `..` is refused. Case is left alone — the record
/// keeps what was written — and symbolic links are not resolved, because
/// re-pointing a link would otherwise change a run's key.
pub fn normalize_relpath(relative: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(nfc(&part.to_string_lossy()));
            }
            std::path::Component::CurDir => {}
            _ => {
                return Err(Error::spec(format!(
                    "`{}` は results からの相対パスとして使えません",
                    relative.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(Error::spec("results 直下そのものは run ではありません"));
    }
    Ok(parts.join("/"))
}

fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// Finds every legacy run under `results_root`.
///
/// Descends through grouping directories and stops at the first directory whose
/// name is a legacy run name, so a sweep's per-condition subdirectories stay
/// part of their parent rather than becoming runs of their own. Directories that
/// hold a `run.json` belong to [`crate::paths::run_dirs`], not here.
pub fn find_run_dirs(results_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !results_root.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![results_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
            // `latest` and `latest_finished` are links to runs already listed.
            if kind.is_symlink() || !kind.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if path.join("run.json").is_file() {
                continue;
            }
            if parse_dir_name(&name).is_some() {
                out.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Reads every legacy run under `results_root`.
pub fn read_all(results_root: &Path, repo_id: &str) -> Result<Vec<LegacyRun>> {
    find_run_dirs(results_root)?
        .into_iter()
        .map(|dir| read_run(results_root, repo_id, &dir))
        .collect()
}

/// Reads one legacy run.
///
/// `results_root` and `repo_id` are passed in rather than inferred: the key has
/// to be the same string on every machine, and guessing where `results/` begins
/// would make it depend on where the reader was standing.
pub fn read_run(results_root: &Path, repo_id: &str, run_dir: &Path) -> Result<LegacyRun> {
    let relative = run_dir.strip_prefix(results_root).map_err(|_| {
        Error::spec(format!(
            "{} は {} の下にありません",
            run_dir.display(),
            results_root.display()
        ))
    })?;
    let relpath = normalize_relpath(relative)?;

    let name = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let parsed = parse_dir_name(&name);

    let mut notes = Vec::new();
    let (timestamp, subcommand) = match &parsed {
        Some(parts) => {
            let subcommand = match (&parts.prefix, &parts.suffix) {
                (Some(prefix), Some(suffix)) => {
                    notes.push(format!(
                        "ディレクトリ名に前置 `{prefix}` と後置 `{suffix}` の両方があります．前置を subcommand として読みました"
                    ));
                    Some(prefix.clone())
                }
                (Some(prefix), None) => Some(prefix.clone()),
                (None, Some(suffix)) => Some(suffix.clone()),
                (None, None) => None,
            };
            (Some(parts.timestamp.clone()), subcommand)
        }
        None => {
            notes.push(format!("`{name}` は legacy の命名に当てはまりません"));
            (None, None)
        }
    };

    let experiment = {
        let mut parts: Vec<&str> = relpath.split('/').collect();
        parts.pop();
        (!parts.is_empty()).then(|| parts.join("/"))
    };

    let parameters = read_parameters(run_dir, &mut notes);
    let (metrics, tables, extras) = read_files(run_dir, &mut notes)?;

    Ok(LegacyRun {
        run_key: run_key(repo_id, &relpath),
        repo_id: repo_id.to_string(),
        relpath,
        dir: run_dir.to_path_buf(),
        experiment,
        subcommand,
        timestamp,
        parameters,
        metrics,
        tables,
        extras,
        notes,
    })
}

/// `config.json` as written: a flat object, with none of the envelope around it.
fn read_parameters(run_dir: &Path, notes: &mut Vec<String>) -> Option<Value> {
    let path = run_dir.join("config.json");
    if !path.is_file() {
        return None;
    }
    match files::read_json::<Value>(&path) {
        Ok(value) if value.is_object() => Some(value),
        Ok(_) => {
            notes.push("config.json が JSON オブジェクトではないので読みませんでした".into());
            None
        }
        Err(e) => {
            notes.push(format!("config.json を読めませんでした: {e}"));
            None
        }
    }
}

type ReadFiles = (Vec<LegacyMetric>, Vec<LegacyTable>, Vec<String>);

fn read_files(run_dir: &Path, notes: &mut Vec<String>) -> Result<ReadFiles> {
    let mut metrics = Vec::new();
    let mut tables = Vec::new();
    let mut extras = Vec::new();

    let mut names: Vec<String> = Vec::new();
    let entries = std::fs::read_dir(run_dir).map_err(|e| Error::io(run_dir, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(run_dir, e))?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name.starts_with('.') {
            continue;
        }
        if entry
            .file_type()
            .map_err(|e| Error::io(run_dir, e))?
            .is_file()
        {
            names.push(file_name);
        }
    }
    names.sort();

    for file_name in names {
        if file_name == "config.json" {
            continue;
        }
        let path = run_dir.join(&file_name);
        let stem = file_name.strip_suffix(".csv");
        match stem {
            Some(stem) if is_metric_file(stem) => {
                match convert_wide(&path, &file_name, stem, notes) {
                    Ok(Converted::Metrics(rows)) => metrics.extend(rows),
                    Ok(Converted::Table(table)) => tables.push(table),
                    Err(e) => notes.push(format!("{file_name}: {e}")),
                }
            }
            _ => extras.push(file_name),
        }
    }

    Ok((metrics, tables, extras))
}

enum Converted {
    Metrics(Vec<LegacyMetric>),
    Table(LegacyTable),
}

/// Turns one wide CSV into long rows, or explains why it stayed a table.
///
/// A file becomes metrics only when the result has a unique
/// `(name, step, step_unit, scope)` key, which is what the index requires. A
/// sweep summary has one row per condition and no time axis, so every row would
/// claim the same key: it stays a table, and the conditions stay readable.
fn convert_wide(
    path: &Path,
    file_name: &str,
    stem: &str,
    notes: &mut Vec<String>,
) -> Result<Converted> {
    let mut reader = csv::Reader::from_path(path).map_err(Error::Csv)?;
    let header: Vec<String> = reader
        .headers()
        .map_err(Error::Csv)?
        .iter()
        .map(str::to_string)
        .collect();
    let rows: Vec<Vec<String>> = reader
        .records()
        .map(|record| record.map(|r| r.iter().map(str::to_string).collect()))
        .collect::<std::result::Result<_, _>>()
        .map_err(Error::Csv)?;

    let table = |reason: &str| {
        Converted::Table(LegacyTable {
            file: file_name.to_string(),
            header: header.clone(),
            rows: rows.len(),
            reason: reason.to_string(),
        })
    };

    let axes: Vec<(usize, &str, &str)> = header
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            TIME_AXIS
                .iter()
                .find(|(column, _)| *column == name)
                .map(|(column, unit)| (i, *column, *unit))
        })
        .collect();

    // Some legacy files are already long (`t,metric,value`). Reading them as if
    // every column were a metric would turn the name column into rows called
    // "value", one per step, which collide immediately.
    if let Some(name_column) = header
        .iter()
        .position(|h| LONG_NAME_COLUMNS.contains(&h.as_str()))
        && let Some(value_column) = long_value_column(&header, name_column)
    {
        return convert_long(
            file_name,
            stem,
            &header,
            &rows,
            name_column,
            value_column,
            &axes,
            notes,
        );
    }

    let axis = axes.first().copied();
    if axes.len() > 1 {
        notes.push(format!(
            "{file_name}: 時間軸になりうる列が {} 個あります．先頭の `{}` を使いました",
            axes.len(),
            axis.unwrap().1
        ));
    }
    if let Some((_, column, unit)) = axis {
        if UNREGISTERED_UNITS.contains(&unit) {
            notes.push(format!(
                "{file_name}: `{column}` を step_unit=`{unit}` として読みました (vocabulary.toml に未登録)"
            ));
        } else if column != unit {
            notes.push(format!(
                "{file_name}: `{column}` 列を step_unit=`{unit}` として読みました"
            ));
        }
    }

    if axis.is_none() && rows.len() > 1 {
        return Ok(table(
            "時間軸の列が無く，行が複数あります (long にすると主キーが重複します)",
        ));
    }

    // Only `metrics.csv` may use bare column names; anything else keeps the file
    // in the name so two files in one run cannot claim the same metric.
    let prefix = (stem != "metrics").then(|| format!("{stem}."));

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped: Vec<String> = Vec::new();

    for row in &rows {
        let step = match axis {
            Some((i, _, _)) => match row.get(i).and_then(|v| v.parse::<i64>().ok()) {
                Some(step) => Some(step),
                None => {
                    skipped.push(format!("{}: 時間軸が整数ではありません", file_name));
                    continue;
                }
            },
            None => None,
        };
        let step_unit = axis.map(|(_, _, unit)| unit.to_string());

        for (i, column) in header.iter().enumerate() {
            if axis.is_some_and(|(axis_i, _, _)| axis_i == i) {
                continue;
            }
            let Some(field) = row.get(i) else { continue };
            let Ok(value) = field.parse::<f64>() else {
                skipped.push(column.clone());
                continue;
            };
            if !value.is_finite() {
                skipped.push(column.clone());
                continue;
            }
            let name = match &prefix {
                Some(prefix) => format!("{prefix}{column}"),
                None => column.clone(),
            };
            if ids::validate_slug("legacy の指標名", &name).is_err() {
                skipped.push(column.clone());
                continue;
            }
            if !seen.insert((name.clone(), step, step_unit.clone())) {
                return Ok(table(&format!(
                    "行の間で主キーが重複します (最初の重複: name={name}, step={})．\
                     系列を分ける列があるなら metrics.csv ではなく events.jsonl の形です",
                    step.map(|s| s.to_string()).unwrap_or_else(|| "なし".into())
                )));
            }
            out.push(LegacyMetric {
                step,
                step_unit: step_unit.clone(),
                scope: "run".into(),
                name,
                value,
                source: file_name.to_string(),
            });
        }
    }

    if !skipped.is_empty() {
        let mut unique: Vec<String> = skipped
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        unique.sort();
        notes.push(format!(
            "{file_name}: 数値でない・名前が使えない列は指標にしませんでした ({})",
            unique.join(", ")
        ));
    }

    if out.is_empty() {
        return Ok(table("指標として読める列がありません"));
    }
    Ok(Converted::Metrics(out))
}

/// Which column holds the value in an already-long file.
///
/// `<name column>_value` wins over a bare `value`: a header such as
/// `kind,topic,dimension,value,n,metric,metric_value` has both, and there
/// `value` belongs to `dimension`, not to `metric`.
fn long_value_column(header: &[String], name_column: usize) -> Option<usize> {
    let paired = format!("{}_value", header[name_column]);
    header
        .iter()
        .position(|h| *h == paired)
        .or_else(|| header.iter().position(|h| h == "value"))
}

/// Reads a legacy file that is already in the long shape.
#[allow(clippy::too_many_arguments)]
fn convert_long(
    file_name: &str,
    stem: &str,
    header: &[String],
    rows: &[Vec<String>],
    name_column: usize,
    value_column: usize,
    axes: &[(usize, &str, &str)],
    notes: &mut Vec<String>,
) -> Result<Converted> {
    let axis = axes
        .iter()
        .find(|(i, _, _)| *i != name_column && *i != value_column)
        .copied();
    let scope_column = header.iter().position(|h| h == "scope");
    let prefix = (stem != "metrics").then(|| format!("{stem}."));

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped: Vec<String> = Vec::new();

    for row in rows {
        let step = match axis {
            Some((i, _, _)) => match row.get(i).and_then(|v| v.parse::<i64>().ok()) {
                Some(step) => Some(step),
                None => {
                    skipped.push("時間軸が整数でない行".into());
                    continue;
                }
            },
            None => None,
        };
        let step_unit = axis.map(|(_, _, unit)| unit.to_string());
        let scope = scope_column
            .and_then(|i| row.get(i))
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| "run".into());

        let Some(raw_name) = row.get(name_column) else {
            continue;
        };
        let name = match &prefix {
            Some(prefix) => format!("{prefix}{raw_name}"),
            None => raw_name.clone(),
        };
        let Some(value) = row.get(value_column).and_then(|v| v.parse::<f64>().ok()) else {
            skipped.push(raw_name.clone());
            continue;
        };
        if !value.is_finite()
            || ids::validate_slug("legacy の指標名", &name).is_err()
            || ids::validate_slug("legacy の scope", &scope).is_err()
        {
            skipped.push(raw_name.clone());
            continue;
        }
        if !seen.insert((name.clone(), step, step_unit.clone(), scope.clone())) {
            return Ok(Converted::Table(LegacyTable {
                file: file_name.to_string(),
                header: header.to_vec(),
                rows: rows.len(),
                reason: format!("long 形式ですが主キーが重複します (最初の重複: name={name})"),
            }));
        }
        out.push(LegacyMetric {
            step,
            step_unit: step_unit.clone(),
            scope,
            name,
            value,
            source: file_name.to_string(),
        });
    }

    if !skipped.is_empty() {
        let mut unique: Vec<String> = skipped
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        unique.sort();
        notes.push(format!(
            "{file_name}: long 形式として読みましたが，値が数値でない・名前が使えない行は落としました ({})",
            unique.join(", ")
        ));
    } else {
        notes.push(format!("{file_name}: long 形式として読みました"));
    }

    if out.is_empty() {
        return Ok(Converted::Table(LegacyTable {
            file: file_name.to_string(),
            header: header.to_vec(),
            rows: rows.len(),
            reason: "long 形式ですが指標として読める行がありません".into(),
        }));
    }
    Ok(Converted::Metrics(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_four_legacy_name_shapes_are_accepted() {
        let bare = parse_dir_name("20260620_134109").unwrap();
        assert_eq!(bare.timestamp, "20260620_134109");
        assert_eq!(bare.prefix, None);
        assert_eq!(bare.suffix, None);

        let suffixed = parse_dir_name("20260620_162729_sweep").unwrap();
        assert_eq!(suffixed.suffix.as_deref(), Some("sweep"));
        assert_eq!(suffixed.prefix, None);

        let prefixed = parse_dir_name("reproduce_20260530_220555").unwrap();
        assert_eq!(prefixed.prefix.as_deref(), Some("reproduce"));
        assert_eq!(prefixed.suffix, None);

        let both = parse_dir_name("generate_20260615_115246_v2").unwrap();
        assert_eq!(both.prefix.as_deref(), Some("generate"));
        assert_eq!(both.suffix.as_deref(), Some("v2"));
    }

    #[test]
    fn a_name_without_a_timestamp_is_not_a_run() {
        assert_eq!(parse_dir_name("paper_reproduction"), None);
        assert_eq!(parse_dir_name("figures"), None);
        assert_eq!(parse_dir_name("tau_0.400_vac_0.300_seed_42"), None);
        assert_eq!(parse_dir_name("2026062_134109"), None);
        assert_eq!(parse_dir_name("20260620_13410"), None);
    }

    #[test]
    fn a_specification_run_name_is_also_a_legacy_name_but_is_filtered_by_run_json() {
        // `main_20260830_101500_9f2c41ab_3b1d` parses; `find_run_dirs` is what
        // keeps a proper run out, by looking for run.json.
        assert!(parse_dir_name("main_20260830_101500_9f2c41ab_3b1d").is_some());
    }

    #[test]
    fn the_key_encodes_the_characters_that_would_split_it() {
        assert_eq!(run_key("repo", "a/b"), "legacy:repo:a/b");
        assert_eq!(run_key("re:po", "a%b:c"), "legacy:re%3Apo:a%25b%3Ac");
        // Encoding order matters: a literal `%` must not swallow an encoded colon.
        assert_ne!(run_key("a", "%3A"), run_key("a", ":"));
    }

    #[test]
    fn relative_paths_are_normalized_but_not_case_folded() {
        assert_eq!(normalize_relpath(Path::new("./a/./B")).unwrap(), "a/B");
        assert!(normalize_relpath(Path::new("../a")).is_err());
        assert!(normalize_relpath(Path::new("")).is_err());
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn a_wide_metrics_file_becomes_long_rows() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(&run, "config.json", r#"{"rows": 13, "seed": 42}"#);
        write(
            &run,
            "metrics.csv",
            "step,avg_same_ratio,n_moved\n0,0.60,0\n1,0.82,88\n",
        );

        let read = read_run(root.path(), "schelling1971", &run).unwrap();
        assert_eq!(read.run_key, "legacy:schelling1971:20260620_134109");
        assert_eq!(read.timestamp.as_deref(), Some("20260620_134109"));
        assert_eq!(read.subcommand, None);
        assert_eq!(read.parameters.as_ref().unwrap()["rows"], 13);
        assert_eq!(read.metrics.len(), 4);

        let first = &read.metrics[0];
        assert_eq!(first.name, "avg_same_ratio");
        assert_eq!(first.step, Some(0));
        assert_eq!(first.step_unit.as_deref(), Some("step"));
        assert_eq!(first.scope, "run");
    }

    #[test]
    fn a_t_column_is_a_time_axis_and_the_reading_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(&run, "metrics.csv", "t,opinion\n0,0.5\n1,0.6\n");
        let read = read_run(root.path(), "r", &run).unwrap();
        assert_eq!(read.metrics.len(), 2);
        assert_eq!(read.metrics[0].step_unit.as_deref(), Some("step"));
        assert!(
            read.notes.iter().any(|n| n.contains("`t` 列")),
            "{:?}",
            read.notes
        );
    }

    #[test]
    fn a_column_that_merely_comes_first_is_not_a_time_axis() {
        // Real files start with `experiment`, `seed` or `respondent_id`.
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(&run, "metrics.csv", "experiment,score\na,0.5\nb,0.6\n");
        let read = read_run(root.path(), "r", &run).unwrap();
        assert!(read.metrics.is_empty());
        assert_eq!(read.tables.len(), 1);
        assert!(
            read.tables[0].reason.contains("主キー"),
            "{:?}",
            read.tables
        );
    }

    #[test]
    fn a_sweep_summary_stays_a_table_because_its_rows_would_collide() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_162729_sweep");
        write(
            &run,
            "sweep_summary.csv",
            "threshold,vacant_rate,converged,avg_same_ratio\n0.4,0.3,true,0.76\n0.45,0.3,true,0.81\n",
        );
        let read = read_run(root.path(), "schelling1971", &run).unwrap();
        assert_eq!(read.subcommand.as_deref(), Some("sweep"));
        assert!(read.metrics.is_empty());
        assert_eq!(read.tables.len(), 1);
        assert_eq!(read.tables[0].rows, 2);
    }

    #[test]
    fn a_single_row_summary_is_an_aggregate_and_does_convert() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_162729_sweep");
        write(
            &run,
            "sweep_summary.csv",
            "converged,avg_same_ratio\ntrue,0.76\n",
        );
        let read = read_run(root.path(), "r", &run).unwrap();
        assert_eq!(read.metrics.len(), 1);
        assert_eq!(read.metrics[0].name, "sweep_summary.avg_same_ratio");
        assert_eq!(read.metrics[0].step, None);
        assert_eq!(read.metrics[0].step_unit, None);
        // `converged` is not a number, and that is said out loud.
        assert!(
            read.notes.iter().any(|n| n.contains("converged")),
            "{:?}",
            read.notes
        );
    }

    #[test]
    fn two_metric_files_in_one_run_do_not_claim_the_same_name() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(&run, "metrics.csv", "step,score\n0,0.5\n");
        write(&run, "metrics_none.csv", "step,score\n0,0.9\n");
        let read = read_run(root.path(), "r", &run).unwrap();
        let names: Vec<&str> = read.metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"score"));
        assert!(names.contains(&"metrics_none.score"));
    }

    #[test]
    fn a_file_already_in_the_long_shape_is_read_as_long() {
        // 57 real files look exactly like this.
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(
            &run,
            "metrics.csv",
            "t,metric,value\n0,polarization_index,0.315\n0,opinion_std,0.561\n1,polarization_index,0.4\n",
        );
        let read = read_run(root.path(), "r", &run).unwrap();
        assert_eq!(read.metrics.len(), 3);
        assert_eq!(read.metrics[0].name, "polarization_index");
        assert_eq!(read.metrics[0].step, Some(0));
        assert_eq!(read.metrics[0].step_unit.as_deref(), Some("step"));
        assert_eq!(read.metrics[0].value, 0.315);
        // Read as wide it would have produced rows called "value" that collide.
        assert!(read.metrics.iter().all(|m| m.name != "value"));
    }

    #[test]
    fn a_paired_value_column_wins_over_a_bare_one() {
        // `value` here belongs to `dimension`, not to `metric`.
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(
            &run,
            "metrics.csv",
            "kind,dimension,value,metric,metric_value\na,d1,0.1,kl_divergence,0.9\n",
        );
        let read = read_run(root.path(), "r", &run).unwrap();
        assert_eq!(read.metrics.len(), 1);
        assert_eq!(read.metrics[0].name, "kl_divergence");
        assert_eq!(read.metrics[0].value, 0.9);
    }

    #[test]
    fn a_long_file_whose_rows_collide_stays_a_table() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(&run, "metrics.csv", "t,metric,value\n0,a,1\n0,a,2\n");
        let read = read_run(root.path(), "r", &run).unwrap();
        assert!(read.metrics.is_empty());
        assert!(
            read.tables[0].reason.contains("long 形式"),
            "{:?}",
            read.tables
        );
    }

    #[test]
    fn a_long_file_may_carry_its_own_scope() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_134109");
        write(
            &run,
            "metrics.csv",
            "metric,value,scope\nasr,0.21,run\nasr,0.3,trial\n",
        );
        let read = read_run(root.path(), "r", &run).unwrap();
        assert_eq!(read.metrics.len(), 2);
        assert_eq!(read.metrics[0].scope, "run");
        assert_eq!(read.metrics[1].scope, "trial");
    }

    #[test]
    fn files_that_are_not_metrics_are_listed_rather_than_interpreted() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260530_223136");
        write(&run, "metrics.csv", "step,score\n0,0.5\n");
        write(&run, "opinions.csv", "t,agent,opinion\n0,a,0.5\n");
        write(&run, "run_metadata.json", r#"{"llm_model": "llama3.2"}"#);
        let read = read_run(root.path(), "r", &run).unwrap();
        assert_eq!(read.extras, ["opinions.csv", "run_metadata.json"]);
    }

    #[test]
    fn a_grouping_directory_is_descended_and_a_run_is_not() {
        let root = tempfile::tempdir().unwrap();
        write(
            &root.path().join("20260620_134109"),
            "metrics.csv",
            "step,a\n0,1\n",
        );
        write(
            &root.path().join("paper_reproduction/20260717_111500"),
            "metrics.csv",
            "step,a\n0,1\n",
        );
        // A sweep's per-condition directory belongs to the sweep, not to the index.
        write(
            &root
                .path()
                .join("20260620_162729_sweep/tau_0.400_vac_0.300_seed_42"),
            "metrics.csv",
            "step,a\n0,1\n",
        );

        let dirs = find_run_dirs(root.path()).unwrap();
        let names: Vec<String> = dirs
            .iter()
            .map(|d| {
                d.strip_prefix(root.path())
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_eq!(
            names,
            [
                "20260620_134109",
                "20260620_162729_sweep",
                "paper_reproduction/20260717_111500"
            ]
        );

        let runs = read_all(root.path(), "schelling1971").unwrap();
        assert_eq!(runs[2].experiment.as_deref(), Some("paper_reproduction"));
        assert_eq!(runs[0].experiment, None);
    }

    #[test]
    fn a_latest_link_and_a_dot_directory_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20260620_165430");
        write(&run, "metrics.csv", "step,a\n0,1\n");
        std::os::unix::fs::symlink("20260620_165430", root.path().join("latest")).unwrap();
        std::fs::write(root.path().join(".DS_Store"), "junk").unwrap();
        std::fs::create_dir_all(root.path().join(".hidden/20260101_000000")).unwrap();

        let dirs = find_run_dirs(root.path()).unwrap();
        assert_eq!(dirs.len(), 1);
        assert!(dirs[0].ends_with("20260620_165430"));
    }

    #[test]
    fn a_run_written_to_specification_is_not_read_as_legacy() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("main_20260830_101500_9f2c41ab_3b1d");
        write(&run, "run.json", "{}");
        assert!(find_run_dirs(root.path()).unwrap().is_empty());
    }

    #[test]
    fn every_legacy_run_gets_a_distinct_key() {
        let root = tempfile::tempdir().unwrap();
        write(
            &root.path().join("20260828_143520_reproduce"),
            "metrics.csv",
            "step,a\n0,1\n",
        );
        write(
            &root.path().join("20260828_143520_ablation"),
            "metrics.csv",
            "step,a\n0,1\n",
        );
        let runs = read_all(root.path(), "r").unwrap();
        assert_eq!(runs.len(), 2);
        assert_ne!(runs[0].run_key, runs[1].run_key);
    }
}
