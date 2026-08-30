//! `runvault verify` — the invariants that span files.
//!
//! A JSON Schema sees one file, or one line, at a time. Whether a run
//! contradicts *itself* is a different question, and this is where it is asked
//! (design note §3.10). The shallow checks are cheap enough to run at the end of
//! every execution; rehashing the data and walking `events.jsonl` are not, and
//! belong before a sync or before a table is built.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::config::{ConfigEnvelope, Exclusions};
use crate::error::{Error, Result};
use crate::files;
use crate::ids;
use crate::meta::{Lineage, Research, RunMeta};
use crate::status::{RunStatus, State};

/// Runs every shallow invariant against a run directory.
pub fn shallow(run_dir: &Path) -> Result<()> {
    let meta: RunMeta = files::read_json(&run_dir.join("run.json"))?;
    let uid = meta.run_uid.as_str();

    check_slug_matches_hashes(run_dir, &meta)?;
    check_config(run_dir, &meta)?;
    check_status(run_dir, uid)?;
    check_metrics(run_dir, uid)?;
    check_reference(run_dir, &meta)?;
    check_manifest(run_dir, uid)?;
    check_events(run_dir, uid)?;
    check_research(&meta.research)?;
    check_data(&meta)?;
    check_lineage(run_dir, &meta)?;
    Ok(())
}

/// The directory name, `run_slug` and the two hashes must all agree.
fn check_slug_matches_hashes(run_dir: &Path, meta: &RunMeta) -> Result<()> {
    let name = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name != meta.run_slug {
        return Err(Error::verify(format!(
            "ディレクトリ名 `{name}` が run_slug `{}` と違います",
            meta.run_slug
        )));
    }
    let Some((cfg8, exec4)) = ids::slug_hash_prefixes(&meta.run_slug) else {
        return Err(Error::verify(format!(
            "run_slug `{}` からハッシュの接頭を読み取れません",
            meta.run_slug
        )));
    };
    if !meta.config_hash.starts_with(cfg8) {
        return Err(Error::verify(format!(
            "run_slug の cfg8 `{cfg8}` が config_hash `{}` と食い違います",
            meta.config_hash
        )));
    }
    if !meta.execution_hash.starts_with(exec4) {
        return Err(Error::verify(format!(
            "run_slug の exec4 `{exec4}` が execution_hash `{}` と食い違います",
            meta.execution_hash
        )));
    }
    Ok(())
}

fn check_config(run_dir: &Path, meta: &RunMeta) -> Result<()> {
    let path = run_dir.join("config.json");
    let config: ConfigEnvelope = files::read_json(&path)?;
    if config.run_uid != meta.run_uid {
        return Err(Error::verify(format!(
            "config.json の run_uid `{}` が run.json `{}` と違います",
            config.run_uid, meta.run_uid
        )));
    }
    // A pointer that no longer resolves means an exclusion silently stopped applying.
    Exclusions::resolve(&config.runvault, &config.parameters)?;
    Ok(())
}

fn check_status(run_dir: &Path, uid: &str) -> Result<()> {
    let path = run_dir.join("status.json");
    if !path.is_file() {
        return Ok(());
    }
    let status: RunStatus = files::read_json(&path)?;
    if status.run_uid != uid {
        return Err(Error::verify(format!(
            "status.json の run_uid `{}` が run.json `{uid}` と違います",
            status.run_uid
        )));
    }
    if status.state == State::Failed && status.error.is_none() {
        return Err(Error::verify(
            "state=failed なのに error がありません".to_string(),
        ));
    }
    if run_dir.join(crate::lockfile::LOCK_FILE).is_file() {
        return Err(Error::verify(
            "status.json があるのに .runvault.lock が残っています (完了した run が実行中に見えます)"
                .to_string(),
        ));
    }
    Ok(())
}

/// A CSV read into memory: the header, then the rows as raw fields.
type CsvTable = (Vec<String>, Vec<Vec<String>>);

/// Reads a CSV into `(header, rows)`, or `None` when the file is absent.
fn read_csv(path: &Path) -> Result<Option<CsvTable>> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut reader = csv::Reader::from_path(path).map_err(Error::Csv)?;
    let header: Vec<String> = reader
        .headers()
        .map_err(Error::Csv)?
        .iter()
        .map(str::to_string)
        .collect();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(Error::Csv)?;
        rows.push(record.iter().map(str::to_string).collect());
    }
    Ok(Some((header, rows)))
}

fn column<'a>(header: &[String], row: &'a [String], name: &str) -> &'a str {
    header
        .iter()
        .position(|h| h == name)
        .and_then(|i| row.get(i))
        .map(String::as_str)
        .unwrap_or_default()
}

fn check_uid_column(file: &str, header: &[String], rows: &[Vec<String>], uid: &str) -> Result<()> {
    for (i, row) in rows.iter().enumerate() {
        let found = column(header, row, "run_uid");
        if found != uid {
            return Err(Error::verify(format!(
                "{file} の {} 行目の run_uid `{found}` が run.json `{uid}` と違います",
                i + 2
            )));
        }
    }
    Ok(())
}

fn check_unique(file: &str, keys: impl IntoIterator<Item = Vec<String>>) -> Result<()> {
    let mut seen = HashSet::new();
    for key in keys {
        if !seen.insert(key.clone()) {
            return Err(Error::verify(format!(
                "{file} に主キーの重複があります: {}",
                key.join(", ")
            )));
        }
    }
    Ok(())
}

fn check_metrics(run_dir: &Path, uid: &str) -> Result<()> {
    let Some((header, rows)) = read_csv(&run_dir.join("metrics.csv"))? else {
        return Ok(());
    };
    check_uid_column("metrics.csv", &header, &rows, uid)?;
    check_unique(
        "metrics.csv",
        rows.iter().map(|row| {
            ["run_uid", "name", "step", "step_unit", "scope"]
                .iter()
                .map(|c| column(&header, row, c).to_string())
                .collect()
        }),
    )
}

fn check_reference(run_dir: &Path, meta: &RunMeta) -> Result<()> {
    let Some((header, rows)) = read_csv(&run_dir.join("reference.csv"))? else {
        return Ok(());
    };
    check_uid_column("reference.csv", &header, &rows, &meta.run_uid)?;
    check_unique(
        "reference.csv",
        rows.iter().map(|row| {
            ["run_uid", "name", "step", "step_unit", "scope", "target_id"]
                .iter()
                .map(|c| column(&header, row, c).to_string())
                .collect()
        }),
    )?;

    let known: BTreeSet<&str> = meta
        .research
        .targets
        .iter()
        .map(|t| t.target_id.as_str())
        .collect();
    for row in &rows {
        let target = column(&header, row, "target_id");
        if !known.contains(target) {
            return Err(Error::verify(format!(
                "reference.csv の target_id `{target}` が research.targets[] にありません"
            )));
        }
    }
    Ok(())
}

fn check_manifest(run_dir: &Path, uid: &str) -> Result<()> {
    let Some((header, rows)) = read_csv(&run_dir.join("manifest.csv"))? else {
        return Ok(());
    };
    check_uid_column("manifest.csv", &header, &rows, uid)?;
    check_unique(
        "manifest.csv",
        rows.iter().map(|row| {
            vec![
                column(&header, row, "run_uid").into(),
                column(&header, row, "path").into(),
            ]
        }),
    )
}

fn check_events(run_dir: &Path, uid: &str) -> Result<()> {
    let path = run_dir.join("events.jsonl");
    if !path.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            Error::verify(format!(
                "events.jsonl の {} 行目が JSON ではありません: {e}",
                i + 1
            ))
        })?;
        let found = value
            .get("run_uid")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if found != uid {
            return Err(Error::verify(format!(
                "events.jsonl の {} 行目の run_uid `{found}` が run.json `{uid}` と違います",
                i + 1
            )));
        }
    }
    Ok(())
}

fn check_data(meta: &RunMeta) -> Result<()> {
    let mut seen = BTreeSet::new();
    for d in &meta.data {
        if !seen.insert((&d.role, &d.name)) {
            return Err(Error::verify(format!(
                "data[] の (role, name) が重複しています: ({}, {})",
                d.role, d.name
            )));
        }
        if d.hash.is_some() != d.hash_scope.is_some() {
            return Err(Error::verify(format!(
                "data[] の ({}, {}) は hash と hash_scope を対で持つ必要があります",
                d.role, d.name
            )));
        }
    }
    Ok(())
}

/// The replication is identified, and the id it claims agrees with the ids it carries.
pub fn check_research(research: &Research) -> Result<()> {
    let mut seen = BTreeSet::new();
    for target in &research.targets {
        ids::validate_slug("target_id", &target.target_id)?;
        if !seen.insert(target.target_id.as_str()) {
            return Err(Error::verify(format!(
                "research.targets[] の target_id `{}` が重複しています",
                target.target_id
            )));
        }
    }

    if !research.is_replication {
        return Ok(());
    }

    let work = research
        .work
        .as_ref()
        .ok_or_else(|| Error::spec("再現実験では work が必要です"))?;
    if work.title.trim().is_empty() {
        return Err(Error::spec("再現実験では work.title が必要です"));
    }
    if work
        .source_version
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        return Err(Error::spec(
            "再現実験では work.source_version が必要です (同じ表でも版で数値が変わるため)",
        ));
    }
    if research.targets.is_empty() {
        return Err(Error::spec("再現実験では targets[] が 1 件以上必要です"));
    }
    if research
        .obsidian_note
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        return Err(Error::spec("再現実験では obsidian_note が必要です"));
    }

    // The prefix of work_id names which of the three ids is the canonical one.
    let (prefix, rest) = work
        .work_id
        .split_once(':')
        .ok_or_else(|| Error::spec(format!("work_id `{}` に接頭辞がありません", work.work_id)))?;
    let declared = match prefix {
        "doi" => work.doi.as_deref(),
        "arxiv" => work.arxiv_id.as_deref(),
        "paperid" => work.paper_id.as_deref(),
        other => {
            return Err(Error::spec(format!(
                "work_id の接頭辞 `{other}` は doi / arxiv / paperid のいずれでもありません"
            )));
        }
    };
    match declared {
        Some(value) if value == rest => Ok(()),
        Some(value) => Err(Error::verify(format!(
            "work_id `{}` と {prefix} `{value}` が食い違います",
            work.work_id
        ))),
        None => Err(Error::spec(format!(
            "work_id が `{prefix}:` なのに {prefix} の欄が空です"
        ))),
    }
}

/// The two lineage rules a schema can express, checked before the run starts.
pub fn check_lineage_shape(lineage: Option<&Lineage>) -> Result<()> {
    let Some(lineage) = lineage else {
        return Ok(());
    };
    if lineage.parent_run_uid.is_some() && lineage.sweep_id.is_none() {
        return Err(Error::spec(
            "parent_run_uid を持つなら sweep_id も必要です (親だけ指して sweep に属さない状態は作らない)",
        ));
    }
    if lineage.resumed_from.is_some() && lineage.derived_from.is_some() {
        return Err(Error::spec(
            "resumed_from と derived_from は同時に立てられません (続きなのか作り直しなのかが決まらなくなります)",
        ));
    }
    Ok(())
}

/// Self-reference, cycles, and what `resumed_from` is allowed to point at.
fn check_lineage(run_dir: &Path, meta: &RunMeta) -> Result<()> {
    check_lineage_shape(meta.lineage.as_ref())?;
    let Some(lineage) = &meta.lineage else {
        return Ok(());
    };

    for (field, target) in [
        ("parent_run_uid", &lineage.parent_run_uid),
        ("resumed_from", &lineage.resumed_from),
        ("derived_from", &lineage.derived_from),
    ] {
        if target.as_deref() == Some(meta.run_uid.as_str()) {
            return Err(Error::verify(format!(
                "lineage.{field} が自分自身を指しています"
            )));
        }
    }

    let siblings = sibling_runs(run_dir);

    if let Some(from) = &lineage.resumed_from
        && let Some(dir) = siblings.get(from.as_str())
    {
        let status: Option<RunStatus> = files::read_json(&dir.join("status.json")).ok();
        match status.map(|s| s.state) {
            Some(State::Failed) | None => {}
            Some(State::Finished) => {
                return Err(Error::verify(format!(
                    "resumed_from `{from}` は finished です (正常終了した run の続きは再解析なので derived_from で表します)"
                )));
            }
        }
    }

    let mut seen = HashSet::new();
    seen.insert(meta.run_uid.clone());
    let mut cursor = lineage
        .resumed_from
        .clone()
        .or_else(|| lineage.derived_from.clone());
    while let Some(uid) = cursor {
        if !seen.insert(uid.clone()) {
            return Err(Error::verify(format!(
                "lineage の鎖が循環しています ({uid})"
            )));
        }
        let Some(dir) = siblings.get(uid.as_str()) else {
            break;
        };
        let Ok(other) = files::read_json::<RunMeta>(&dir.join("run.json")) else {
            break;
        };
        cursor = other
            .lineage
            .and_then(|l| l.resumed_from.or(l.derived_from));
    }
    Ok(())
}

/// The runs reachable from the same results root, by `run_uid`.
///
/// A run may reference one that lives elsewhere; those simply cannot be checked
/// here, which is different from being wrong.
fn sibling_runs(run_dir: &Path) -> HashMap<String, std::path::PathBuf> {
    let Some(results_root) = run_dir.parent().and_then(Path::parent) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for dir in crate::paths::run_dirs(results_root).unwrap_or_default() {
        if let Ok(meta) = files::read_json::<RunMeta>(&dir.join("run.json")) {
            out.insert(meta.run_uid, dir);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{Target, Work};

    fn replication() -> Research {
        Work::doi("10.1080/0022250X.1971.9989794")
            .title("Dynamic Models of Segregation")
            .source_version("published")
            .target(Target::table("tbl3-r2", "Table 3").row("2"))
            .obsidian_note("研究/98_論文レポート/80-再現実験/P00000009/設計書.md")
            .into()
    }

    #[test]
    fn a_complete_replication_passes() {
        check_research(&replication()).unwrap();
    }

    #[test]
    fn a_replication_without_a_source_version_fails() {
        let mut research = replication();
        research.work.as_mut().unwrap().source_version = None;
        assert!(check_research(&research).is_err());
    }

    #[test]
    fn a_replication_without_a_target_fails() {
        let mut research = replication();
        research.targets.clear();
        assert!(check_research(&research).is_err());
    }

    #[test]
    fn a_replication_without_a_note_fails() {
        let mut research = replication();
        research.obsidian_note = None;
        assert!(check_research(&research).is_err());
    }

    #[test]
    fn a_work_id_that_disagrees_with_its_doi_fails() {
        let mut research = replication();
        research.work.as_mut().unwrap().doi = Some("10.9999/other".into());
        let err = check_research(&research).unwrap_err();
        assert!(err.to_string().contains("食い違います"), "{err}");
    }

    #[test]
    fn duplicate_target_ids_fail() {
        let mut research = replication();
        research.targets.push(Target::table("tbl3-r2", "Table 3"));
        assert!(check_research(&research).is_err());
    }

    #[test]
    fn a_run_that_reproduces_nothing_needs_no_paper() {
        check_research(&Research::default()).unwrap();
    }

    #[test]
    fn a_parent_without_a_sweep_fails() {
        let lineage = Lineage {
            sweep_id: None,
            parent_run_uid: Some("01K3QZ8F7H9M2N4P6R8T0V2X4Z".into()),
            resumed_from: None,
            derived_from: None,
        };
        assert!(check_lineage_shape(Some(&lineage)).is_err());
    }

    #[test]
    fn resuming_and_deriving_at_once_fails() {
        let lineage = Lineage {
            sweep_id: None,
            parent_run_uid: None,
            resumed_from: Some("01K3QZ8F7H9M2N4P6R8T0V2X4Z".into()),
            derived_from: Some("01K3QZ8F7H9M2N4P6R8T0V2X50".into()),
        };
        assert!(check_lineage_shape(Some(&lineage)).is_err());
    }

    #[test]
    fn a_sweep_child_that_names_its_sweep_passes() {
        let lineage = Lineage {
            sweep_id: Some("sweep-2026-08-30".into()),
            parent_run_uid: Some("01K3QZ8F7H9M2N4P6R8T0V2X4Z".into()),
            resumed_from: None,
            derived_from: None,
        };
        check_lineage_shape(Some(&lineage)).unwrap();
    }
}
