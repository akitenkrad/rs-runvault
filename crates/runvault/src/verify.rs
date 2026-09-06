//! `runvault verify` — the invariants that span files.
//!
//! A JSON Schema sees one file, or one line, at a time. Whether a run
//! contradicts *itself* is a different question, and this is where it is asked
//! (design note §3.10). The shallow checks are cheap enough to run at the end of
//! every execution; rehashing the data and walking `events.jsonl` are not, and
//! belong before a sync or before a table is built.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::config::{ConfigEnvelope, Exclusions};
use crate::error::{Error, Result};
use crate::files;
use crate::hash;
use crate::ids;
use crate::meta::{Algorithm, Lineage, Research, RunMeta};
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
    for_each_event(&run_dir.join("events.jsonl"), |line, value| {
        let found = value
            .get("run_uid")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if found != uid {
            return Err(Error::verify(format!(
                "events.jsonl の {line} 行目の run_uid `{found}` が run.json `{uid}` と違います"
            )));
        }
        Ok(())
    })
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

    // A run that cannot be found may live in another repository; that is not the
    // same as one that is found and does not say it failed.
    if let Some(from) = &lineage.resumed_from
        && let Some(dir) = siblings.get(from.as_str())
    {
        match files::read_json::<RunStatus>(&dir.join("status.json")) {
            Ok(status) if status.state == State::Failed => {}
            Ok(_) => {
                return Err(Error::verify(format!(
                    "resumed_from `{from}` は finished です (正常終了した run の続きは再解析なので derived_from で表します)"
                )));
            }
            Err(e) => {
                return Err(Error::verify(format!(
                    "resumed_from `{from}` が failed であることを確かめられません: {e}"
                )));
            }
        }
    }

    check_no_cycle(&meta.run_uid, lineage, &siblings)
}

/// Whether any lineage edge leads back to a run already on the current path.
///
/// All three edges are followed, not just the resume/derive chain: two runs that
/// name each other as a sweep parent are as unwalkable as a resume loop.
fn check_no_cycle(
    run_uid: &str,
    lineage: &Lineage,
    siblings: &HashMap<String, std::path::PathBuf>,
) -> Result<()> {
    #[derive(PartialEq)]
    enum Mark {
        OnPath,
        Done,
    }

    let edges_of = |l: &Lineage| -> Vec<String> {
        [&l.parent_run_uid, &l.resumed_from, &l.derived_from]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    };

    let mut marks: HashMap<String, Mark> = HashMap::new();
    let mut stack: Vec<(String, bool)> = vec![(run_uid.to_string(), false)];

    while let Some((uid, leaving)) = stack.pop() {
        if leaving {
            marks.insert(uid, Mark::Done);
            continue;
        }
        match marks.get(&uid) {
            Some(Mark::OnPath) => {
                return Err(Error::verify(format!(
                    "lineage の鎖が循環しています ({uid})"
                )));
            }
            Some(Mark::Done) => continue,
            None => {}
        }
        marks.insert(uid.clone(), Mark::OnPath);
        stack.push((uid.clone(), true));

        let next = if uid == run_uid {
            edges_of(lineage)
        } else {
            match siblings
                .get(&uid)
                .map(|dir| files::read_json::<RunMeta>(&dir.join("run.json")))
            {
                // A run outside this results root ends the walk; it is not proof
                // of a cycle, and it is not proof of the absence of one either.
                Some(Ok(other)) => other.lineage.as_ref().map(edges_of).unwrap_or_default(),
                _ => Vec::new(),
            }
        };
        for edge in next {
            stack.push((edge, false));
        }
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

// ---------------------------------------------------------------------------
// The deep checks: everything whose cost scales with the size of the run.
// ---------------------------------------------------------------------------

/// Runs the shallow invariants, then the ones whose cost scales with the run.
///
/// Recomputing the three hashes is what stops a run whose `parameters` were
/// edited afterwards from still claiming the old `config_hash`, and walking
/// `artifacts/` is what stops a generated file from living outside the record.
/// `runvault sync` runs this before it copies anything and refuses to send a run
/// that fails it, so that the aggregation layer never takes in a broken run.
///
/// `events.jsonl` is read twice, once by the shallow `run_uid` check and once
/// here. Both passes stream the file a line at a time, and the cost is small
/// next to rehashing the artifacts.
pub fn deep(run_dir: &Path) -> Result<()> {
    shallow(run_dir)?;
    let meta: RunMeta = files::read_json(&run_dir.join("run.json"))?;
    check_hashes(run_dir, &meta)?;
    check_locks(run_dir, &meta)?;
    check_manifest_contents(run_dir)?;
    check_terminal_events(run_dir)?;
    Ok(())
}

fn compare_hash(field: &str, recorded: &str, recomputed: &str) -> Result<()> {
    if recorded != recomputed {
        return Err(Error::verify(format!(
            "{field} が元データと一致しません (記録 {recorded}, 再計算 {recomputed})"
        )));
    }
    Ok(())
}

/// The three hashes, recomputed from the files they claim to summarize.
///
/// Each recomputed value is fed to the next hash rather than the recorded one:
/// they have just been proved equal, and using the recomputed value keeps the
/// chain identical to the one `Run::start` built.
fn check_hashes(run_dir: &Path, meta: &RunMeta) -> Result<()> {
    let config: ConfigEnvelope = files::read_json(&run_dir.join("config.json"))?;
    let exclusions = Exclusions::resolve(&config.runvault, &config.parameters)?;
    let code = meta.code.as_ref();

    let env_hash = hash::env_hash(
        &meta.env.os,
        &meta.env.arch,
        meta.env.rustc_version.as_deref(),
        meta.env.python_version.as_deref(),
        code.map(|c| c.locks.as_slice()).unwrap_or(&[]),
    );
    compare_hash("env_hash", &meta.env.env_hash, &env_hash)?;

    let config_hash = hash::config_hash(&config.parameters, &exclusions, &meta.data)?;
    compare_hash("config_hash", &meta.config_hash, &config_hash)?;

    let execution_hash = hash::execution_hash(
        &config_hash,
        &config.parameters,
        &exclusions,
        code,
        &env_hash,
    )?;
    compare_hash("execution_hash", &meta.execution_hash, &execution_hash)
}

/// A recorded relative path, resolved without letting it leave the run directory.
fn inside_run(run_dir: &Path, source: &str, rel: &str) -> Result<PathBuf> {
    let path = Path::new(rel);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::verify(format!(
            "{source} の path `{rel}` が run ディレクトリの外を指しています"
        )));
    }
    Ok(run_dir.join(path))
}

/// The lock files under `lock/` still hash to what `env_hash` was told.
///
/// Nothing else covers them: `manifest.csv` records only `artifacts/` and
/// `logs/`, so without this a rewritten `Cargo.lock` would keep an `env_hash`
/// that describes a different set of dependencies.
fn check_locks(run_dir: &Path, meta: &RunMeta) -> Result<()> {
    let Some(code) = &meta.code else {
        return Ok(());
    };
    for lock in &code.locks {
        let path = inside_run(run_dir, "run.json の code.locks[]", &lock.file)?;
        if !path.is_file() {
            return Err(Error::verify(format!(
                "code.locks[] の `{}` が実在しません",
                lock.file
            )));
        }
        let (digest, _) = files::digest_file_as(&path, lock.hash.algorithm)?;
        if digest != lock.hash.value {
            return Err(Error::verify(format!(
                "`{}` のハッシュが run.json と違います (記録 {}, 実体 {digest})",
                lock.file, lock.hash.value
            )));
        }
    }
    Ok(())
}

/// Whether `status.json` records this run as failed.
///
/// Absent means the run has not ended at all, which is not the same as ending
/// badly, so it answers `false`.
fn recorded_as_failed(run_dir: &Path) -> Result<bool> {
    let path = run_dir.join("status.json");
    if !path.is_file() {
        return Ok(false);
    }
    let status: RunStatus = files::read_json(&path)?;
    Ok(status.state == State::Failed)
}

/// Every file `manifest.csv` names exists and still hashes to what it recorded,
/// and nothing under `artifacts/` or `logs/` is missing from it.
///
/// Both halves are needed. Without the first, "that figure came from this run"
/// stops being true; without the second, a generated file can exist that the
/// record never mentions, and the identity of the run is only half guaranteed.
///
/// The second half presupposes a record to be missing from, and that is
/// something `finish()` alone creates: it writes `manifest.csv` first and
/// `status.json` last. So among directories written by the ordinary path, a run
/// recorded as *failed* with no manifest did not reach the seal — its process
/// was killed and `runvault gc` wrote the status, or `Drop` did, or it failed
/// during `start`. What sits under
/// `artifacts/` there is the debris of a write that was interrupted, not a
/// result the record forgot to mention, and no later step can reconcile the
/// two: holding such a run to the invariant refuses it forever, which is how
/// one killed run blocked a whole repository's preservation every day until it
/// was deleted by hand (MYTASK-3202).
///
/// The exemption is deliberately narrow. A `finished` run is held to it
/// whatever it holds — a run claiming to be a result answers for everything it
/// produced. So is a failed run that *did* seal: a file standing beside a
/// manifest that does not name it appeared after the run ended, which is
/// precisely the case this refuses.
///
/// A missing manifest is an *inference* from the writer's ordering, not proof
/// that cannot be forged. Anyone able to edit a run directory can delete
/// `manifest.csv` and set `state` to `failed` to obtain the exemption. That is
/// not a weakness this exemption introduced: neither `manifest.csv` nor the
/// digests are signed, and `verify` asks whether a run contradicts *itself*.
/// It is built to catch accidents — kills, crashes, a file written and never
/// recorded — and does not claim to detect an adversary. Detecting one needs
/// signed records, which is a separate decision.
fn check_manifest_contents(run_dir: &Path) -> Result<()> {
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    let manifest = read_csv(&run_dir.join("manifest.csv"))?;

    if let Some((header, rows)) = &manifest {
        for row in rows {
            let rel = column(header, row, "path");
            let path = inside_run(run_dir, "manifest.csv", rel)?;
            if !path.is_file() {
                return Err(Error::verify(format!(
                    "manifest.csv の path `{rel}` が実在しません"
                )));
            }

            // A row whose function cannot be named is a row that cannot be
            // checked, and a run is not verified because a check was skipped.
            let named = column(header, row, "algorithm");
            let algorithm = Algorithm::parse(named).ok_or_else(|| {
                Error::verify(format!(
                    "manifest.csv の `{rel}` の algorithm `{named}` は照合できません"
                ))
            })?;
            let (digest, bytes) = files::digest_file_as(&path, algorithm)?;

            let recorded_bytes = column(header, row, "bytes");
            if recorded_bytes != bytes.to_string() {
                return Err(Error::verify(format!(
                    "manifest.csv の `{rel}` のバイト数が違います (記録 {recorded_bytes}, 実体 {bytes})"
                )));
            }
            let recorded_digest = column(header, row, "digest");
            if recorded_digest != digest {
                return Err(Error::verify(format!(
                    "manifest.csv の `{rel}` のハッシュが違います (記録 {recorded_digest}, 実体 {digest})"
                )));
            }
            recorded.insert(rel.to_string());
        }
    }

    if manifest.is_none() && recorded_as_failed(run_dir)? {
        return Ok(());
    }

    for sub in ["artifacts", "logs"] {
        for rel in files::walk_files(&run_dir.join(sub), run_dir)? {
            if !recorded.contains(&rel) {
                return Err(Error::verify(format!(
                    "`{rel}` が manifest.csv にありません (記録に載らない生成物です)"
                )));
            }
        }
    }
    Ok(())
}

/// Calls `f` with each non-blank line of `events.jsonl` and its 1-based number.
///
/// The file is streamed rather than read whole: it is the one file in a run
/// whose size has no bound.
fn for_each_event(
    path: &Path,
    mut f: impl FnMut(usize, &serde_json::Value) -> Result<()>,
) -> Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(path, e)),
    };
    for (i, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| Error::io(path, e))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|e| {
            Error::verify(format!(
                "events.jsonl の {} 行目が JSON ではありません: {e}",
                i + 1
            ))
        })?;
        f(i + 1, &value)?;
    }
    Ok(())
}

fn event_str(value: &serde_json::Value, field: &str, line: usize) -> Result<String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::verify(format!(
                "events.jsonl の {line} 行目: {field} が文字列としてありません"
            ))
        })
}

fn event_num(value: &serde_json::Value, field: &str, line: usize) -> Result<f64> {
    value
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| {
            Error::verify(format!(
                "events.jsonl の {line} 行目: {field} が数値としてありません"
            ))
        })
}

/// A `terminal` row agrees with the `observation` rows it summarizes.
///
/// `outcome` is deliberately not re-derived: which outcome a trajectory earned
/// depends on the experiment's judge, not on the file (design note §3.10).
fn check_terminal_events(run_dir: &Path) -> Result<()> {
    let path = run_dir.join("events.jsonl");
    let mut last_observed: HashMap<String, f64> = HashMap::new();
    let mut terminals: Vec<(usize, String, f64, bool, Option<f64>)> = Vec::new();

    for_each_event(&path, |line, value| {
        match value.get("schema").and_then(|v| v.as_str()) {
            Some("observation") => {
                let unit = event_str(value, "unit_id", line)?;
                let t = event_num(value, "t", line)?;
                last_observed
                    .entry(unit)
                    .and_modify(|max| {
                        if t > *max {
                            *max = t;
                        }
                    })
                    .or_insert(t);
            }
            Some("terminal") => {
                let unit = event_str(value, "unit_id", line)?;
                let t = event_num(value, "t", line)?;
                let censored = value
                    .get("censored")
                    .and_then(serde_json::Value::as_bool)
                    .ok_or_else(|| {
                        Error::verify(format!(
                            "events.jsonl の {line} 行目: terminal に censored がありません"
                        ))
                    })?;
                let budget = value.get("budget").and_then(serde_json::Value::as_f64);
                terminals.push((line, unit, t, censored, budget));
            }
            _ => {}
        }
        Ok(())
    })?;

    for (line, unit, t, censored, budget) in terminals {
        let Some(&observed) = last_observed.get(&unit) else {
            return Err(Error::verify(format!(
                "events.jsonl の {line} 行目: terminal の unit_id `{unit}` が observation に現れません"
            )));
        };
        if t != observed {
            return Err(Error::verify(format!(
                "events.jsonl の {line} 行目: terminal の t={t} が unit_id `{unit}` の observation の最大 t={observed} と違います"
            )));
        }
        if censored {
            match budget {
                Some(budget) if budget == t => {}
                Some(budget) => {
                    return Err(Error::verify(format!(
                        "events.jsonl の {line} 行目: censored なのに t={t} が budget={budget} と違います"
                    )));
                }
                None => {
                    return Err(Error::verify(format!(
                        "events.jsonl の {line} 行目: censored なのに budget がありません"
                    )));
                }
            }
        }
    }
    Ok(())
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
