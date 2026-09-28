//! Deliberate removal of recorded runs, with an append-only tombstone first.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::lockfile::{self, Liveness};
use crate::meta::{RunMeta, SCHEMA_VERSION};
use crate::status::{RunStatus, State};
use crate::sync::{Compression, SyncReceipt};
use crate::{env, files, ids};

/// Name of the append-only deletion record below a repository's vault root.
pub const TOMBSTONES: &str = "_deleted.jsonl";

/// A line in `<vault>/<repo_id>/_deleted.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    /// Version of `schema/v1`.
    pub schema_version: String,
    /// Stable identity of the deleted run.
    pub run_uid: String,
    /// Repository the run belonged to.
    pub repo_id: String,
    /// Experiment the run belonged to.
    pub experiment: String,
    /// Human-readable, non-unique directory name.
    pub run_slug: String,
    /// Recorded terminal state at deletion time.
    pub state: State,
    /// Time deletion was authorized.
    pub deleted_at: String,
    /// Host that performed the deletion.
    pub host: String,
    /// Human-supplied reason for deletion.
    pub reason: String,
    /// runvault version that wrote the tombstone.
    pub runvault_version: String,
}

/// How the caller selected runs. Slugs are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// One or more stable run identifiers.
    RunUids(Vec<String>),
    /// Every failed run in one experiment.
    FailedExperiment(String),
}

/// Where a selected run currently exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Both the source and aggregation copy exist.
    Both,
    /// Only the source exists.
    SourceOnly,
    /// Only the aggregation copy exists.
    VaultOnly,
    /// Neither copy exists; an earlier tombstone still identifies the run.
    Neither,
}

/// The externally visible stages, in their required order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteStage {
    /// The tombstone has been durably appended, or was already present.
    Tombstone,
    /// The aggregation copy and its slug link are absent.
    Vault,
    /// The source copy is absent.
    Source,
}

/// A fully resolved deletion, before any file is changed.
#[derive(Debug, Clone)]
pub struct DeletePlan {
    /// Stable identity.
    pub run_uid: String,
    /// Human-readable directory name shown during preview.
    pub run_slug: String,
    /// Experiment name.
    pub experiment: String,
    /// Source directory, when it exists.
    pub source_dir: Option<PathBuf>,
    /// Aggregation directory, when it exists.
    pub vault_dir: Option<PathBuf>,
    /// Tombstone to append if it is not already present.
    pub tombstone: Tombstone,
    vault_root: PathBuf,
    already_tombstoned: bool,
}

impl DeletePlan {
    /// Reports which of the two copies existed while planning.
    pub fn presence(&self) -> Presence {
        match (self.source_dir.is_some(), self.vault_dir.is_some()) {
            (true, true) => Presence::Both,
            (true, false) => Presence::SourceOnly,
            (false, true) => Presence::VaultOnly,
            (false, false) => Presence::Neither,
        }
    }
}

#[derive(Default)]
struct Found {
    meta: Option<RunMeta>,
    status: Option<RunStatus>,
    source: Option<PathBuf>,
    vault: Option<PathBuf>,
    source_unfinished: bool,
}

/// Path of one repository's append-only tombstone file.
pub fn tombstone_path(vault_root: &Path, repo_id: &str) -> PathBuf {
    vault_root.join(repo_id).join(TOMBSTONES)
}

/// Whether a stable run identifier has already been tombstoned.
pub fn contains(vault_root: &Path, repo_id: &str, run_uid: &str) -> Result<bool> {
    Ok(read(vault_root, repo_id)?.contains_key(run_uid))
}

/// Stable identifiers recorded as deleted for one repository.
pub fn run_uids(vault_root: &Path, repo_id: &str) -> Result<BTreeSet<String>> {
    Ok(read(vault_root, repo_id)?.into_keys().collect())
}

fn read(vault_root: &Path, repo_id: &str) -> Result<BTreeMap<String, Tombstone>> {
    let path = tombstone_path(vault_root, repo_id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(Error::io(path, e)),
    };
    let mut out = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let item: Tombstone = serde_json::from_str(line).map_err(|e| {
            Error::spec(format!(
                "{}:{}: 墓標を読めません: {e}",
                path.display(),
                index + 1
            ))
        })?;
        if item.repo_id != repo_id
            || !ids::is_run_uid(&item.run_uid)
            || ids::validate_slug("experiment", &item.experiment).is_err()
            || !valid_run_slug(&item.run_slug)
            || item.reason.trim().is_empty()
        {
            return Err(Error::spec(format!(
                "{}:{}: 墓標の識別子または理由が不正です",
                path.display(),
                index + 1
            )));
        }
        out.entry(item.run_uid.clone()).or_insert(item);
    }
    Ok(out)
}

/// Resolves and validates every target without changing the file tree.
pub fn plan(
    results_root: &Path,
    vault_root: &Path,
    repo_id: &str,
    selection: Selection,
    reason: &str,
    include_succeeded: bool,
) -> Result<Vec<DeletePlan>> {
    ids::validate_slug("repo_id", repo_id)?;
    if reason.trim().is_empty() {
        return Err(Error::spec("--reason は空にできません"));
    }
    let requested: Option<BTreeSet<String>> = match &selection {
        Selection::RunUids(uids) => {
            if uids.is_empty() {
                return Err(Error::spec("--run-uid を 1 件以上指定してください"));
            }
            for uid in uids {
                if !ids::is_run_uid(uid) {
                    return Err(Error::spec(format!("`{uid}` は run_uid ではありません")));
                }
            }
            Some(uids.iter().cloned().collect())
        }
        Selection::FailedExperiment(experiment) => {
            ids::validate_slug("experiment", experiment)?;
            None
        }
    };

    let tombstones = read(vault_root, repo_id)?;
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    scan_sources(results_root, repo_id, &selection, &mut found)?;
    scan_vault(vault_root, repo_id, &selection, &mut found)?;

    let target_uids: BTreeSet<String> = match requested {
        Some(uids) => uids,
        None => found
            .iter()
            .filter(|(_, item)| {
                item.status
                    .as_ref()
                    .is_some_and(|s| s.state == State::Failed)
            })
            .map(|(uid, _)| uid.clone())
            .collect(),
    };

    let mut out = Vec::new();
    for uid in target_uids {
        let existing_tombstone = tombstones.get(&uid);
        let item = found.remove(&uid).unwrap_or_default();
        let meta = item.meta.as_ref();
        let (experiment, run_slug, state) = if let Some(meta) = meta {
            let state = eligible_state(&item, include_succeeded, existing_tombstone.is_some())?;
            (meta.experiment.clone(), meta.run_slug.clone(), state)
        } else if let Some(old) = existing_tombstone {
            (old.experiment.clone(), old.run_slug.clone(), old.state)
        } else {
            return Err(Error::spec(format!(
                "run_uid {uid} は元にも集約先にも墓標にもありません"
            )));
        };
        let tombstone = existing_tombstone.cloned().unwrap_or_else(|| Tombstone {
            schema_version: SCHEMA_VERSION.into(),
            run_uid: uid.clone(),
            repo_id: repo_id.to_string(),
            experiment: experiment.clone(),
            run_slug: run_slug.clone(),
            state,
            deleted_at: Local::now().to_rfc3339(),
            host: env::host(),
            reason: reason.to_string(),
            runvault_version: env!("CARGO_PKG_VERSION").to_string(),
        });
        out.push(DeletePlan {
            run_uid: uid,
            run_slug,
            experiment,
            source_dir: item.source,
            vault_dir: item.vault,
            tombstone,
            vault_root: vault_root.to_path_buf(),
            already_tombstoned: existing_tombstone.is_some(),
        });
    }
    Ok(out)
}

fn eligible_state(
    item: &Found,
    include_succeeded: bool,
    already_tombstoned: bool,
) -> Result<State> {
    if item.source_unfinished {
        let dir = item.source.as_ref().expect("unfinished source has a path");
        let Some(record) = lockfile::read(dir)? else {
            return Err(Error::spec(format!(
                "{}: status.json も lock も無く，状態を判定できません",
                dir.display()
            )));
        };
        return match record.liveness(Local::now(), &env::host()) {
            Liveness::Running => Err(Error::spec(format!(
                "{}: run は実行中なので削除できません",
                dir.display()
            ))),
            Liveness::Stale => Err(Error::spec(format!(
                "{}: lock が stale です．先に `runvault gc` を実行してください",
                dir.display()
            ))),
        };
    }
    let status = item
        .status
        .as_ref()
        .ok_or_else(|| Error::spec("status.json が無く，run の状態を判定できません"))?;
    match status.state {
        State::Failed => Ok(State::Failed),
        State::Finished if include_succeeded || already_tombstoned => Ok(State::Finished),
        State::Finished => Err(Error::spec(format!(
            "run_uid {} は成功済みです．削除するには --include-succeeded が要ります",
            status.run_uid
        ))),
    }
}

fn scan_sources(
    results_root: &Path,
    repo_id: &str,
    selection: &Selection,
    found: &mut BTreeMap<String, Found>,
) -> Result<()> {
    for dir in crate::paths::run_dirs(results_root)? {
        let run_path = dir.join("run.json");
        if !run_path.is_file() {
            reject_selected_legacy(results_root, &dir, selection)?;
            continue;
        }
        let meta: RunMeta = files::read_json(&run_path)?;
        if !selected(selection, &meta.run_uid, &meta.experiment) {
            continue;
        }
        if meta.repo_id != repo_id {
            return Err(Error::spec(format!(
                "{}: run.json の repo_id `{}` が指定された `{repo_id}` と違います",
                dir.display(),
                meta.repo_id
            )));
        }
        validate_meta_path_fields(&meta, &dir)?;
        let status_path = dir.join("status.json");
        let status = status_path
            .is_file()
            .then(|| files::read_json(&status_path))
            .transpose()?;
        let entry = found.entry(meta.run_uid.clone()).or_default();
        merge_meta(entry, meta, &dir)?;
        merge_status(entry, status, &dir)?;
        entry.source_unfinished = !status_path.is_file();
        entry.source = Some(dir);
    }
    Ok(())
}

fn reject_selected_legacy(results_root: &Path, dir: &Path, selection: &Selection) -> Result<()> {
    let status = files::read_json::<RunStatus>(&dir.join("status.json")).ok();
    let experiment = dir
        .strip_prefix(results_root)
        .ok()
        .and_then(|p| p.components().next())
        .and_then(|c| c.as_os_str().to_str());
    let selected = match selection {
        Selection::RunUids(uids) => status
            .as_ref()
            .is_some_and(|s| uids.iter().any(|uid| uid == &s.run_uid)),
        Selection::FailedExperiment(wanted) => {
            experiment == Some(wanted) && status.as_ref().is_some_and(|s| s.state == State::Failed)
        }
    };
    if selected {
        return Err(Error::spec(format!(
            "{}: run.json が無い legacy run は削除対象にできません",
            dir.display()
        )));
    }
    Ok(())
}

fn scan_vault(
    vault_root: &Path,
    repo_id: &str,
    selection: &Selection,
    found: &mut BTreeMap<String, Found>,
) -> Result<()> {
    let repo = vault_root.join(repo_id);
    if !repo.is_dir() {
        return Ok(());
    }
    for experiment_entry in std::fs::read_dir(&repo).map_err(|e| Error::io(&repo, e))? {
        let experiment_entry = experiment_entry.map_err(Error::PlainIo)?;
        if !experiment_entry
            .file_type()
            .map_err(Error::PlainIo)?
            .is_dir()
        {
            continue;
        }
        let experiment = experiment_entry.file_name().to_string_lossy().to_string();
        let experiment_dir = experiment_entry.path();
        for run_entry in
            std::fs::read_dir(&experiment_dir).map_err(|e| Error::io(&experiment_dir, e))?
        {
            let run_entry = run_entry.map_err(Error::PlainIo)?;
            let dir = run_entry.path();
            if !run_entry.file_type().map_err(Error::PlainIo)?.is_dir()
                || run_entry.file_name() == "by-slug"
            {
                continue;
            }
            let uid = run_entry.file_name().to_string_lossy().to_string();
            if !selected(selection, &uid, &experiment) || !ids::is_run_uid(&uid) {
                continue;
            }
            let meta: RunMeta = read_synced(&dir, "run.json")?;
            if meta.repo_id != repo_id || meta.run_uid != uid {
                return Err(Error::spec(format!(
                    "{}: 集約先の run.json と配置が一致しません",
                    dir.display()
                )));
            }
            validate_meta_path_fields(&meta, &dir)?;
            let status = read_synced_optional::<RunStatus>(&dir, "status.json")?;
            let entry = found.entry(uid).or_default();
            merge_meta(entry, meta, &dir)?;
            merge_status(entry, status, &dir)?;
            entry.vault = Some(dir);
        }
    }
    Ok(())
}

fn selected(selection: &Selection, uid: &str, experiment: &str) -> bool {
    match selection {
        Selection::RunUids(uids) => uids.iter().any(|wanted| wanted == uid),
        Selection::FailedExperiment(wanted) => wanted == experiment,
    }
}

fn valid_run_slug(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && ids::slug_hash_prefixes(value).is_some()
}

fn validate_meta_path_fields(meta: &RunMeta, path: &Path) -> Result<()> {
    ids::validate_slug("experiment", &meta.experiment)?;
    if !ids::is_run_uid(&meta.run_uid) || !valid_run_slug(&meta.run_slug) {
        return Err(Error::spec(format!(
            "{}: run.json の run_uid または run_slug が不正です",
            path.display()
        )));
    }
    Ok(())
}

fn merge_meta(entry: &mut Found, meta: RunMeta, path: &Path) -> Result<()> {
    if let Some(existing) = &entry.meta
        && (existing.run_uid != meta.run_uid
            || existing.run_slug != meta.run_slug
            || existing.experiment != meta.experiment)
    {
        return Err(Error::spec(format!(
            "{}: 元と集約先の run.json が一致しません",
            path.display()
        )));
    }
    entry.meta = Some(meta);
    Ok(())
}

fn merge_status(entry: &mut Found, status: Option<RunStatus>, path: &Path) -> Result<()> {
    let Some(status) = status else { return Ok(()) };
    if entry
        .meta
        .as_ref()
        .is_some_and(|meta| meta.run_uid != status.run_uid)
    {
        return Err(Error::spec(format!(
            "{}: run.json と status.json の run_uid が一致しません",
            path.display()
        )));
    }
    if let Some(existing) = &entry.status
        && existing.state != status.state
    {
        return Err(Error::spec(format!(
            "{}: 元と集約先の status.json が一致しません",
            path.display()
        )));
    }
    entry.status = Some(status);
    Ok(())
}

fn read_synced_optional<T: serde::de::DeserializeOwned>(
    dir: &Path,
    name: &str,
) -> Result<Option<T>> {
    let receipt: SyncReceipt = files::read_json(&dir.join(crate::sync::RECEIPT))?;
    let Some(file) = receipt.files.iter().find(|file| file.path == name) else {
        return Ok(None);
    };
    let path = dir.join(&file.stored_path);
    let handle = std::fs::File::open(&path).map_err(|e| Error::io(&path, e))?;
    let mut bytes = Vec::new();
    match file.compression {
        Compression::None => std::io::BufReader::new(handle).read_to_end(&mut bytes),
        Compression::Zstd => zstd::stream::read::Decoder::new(handle)
            .map_err(Error::PlainIo)?
            .read_to_end(&mut bytes),
    }
    .map_err(Error::PlainIo)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn read_synced<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<T> {
    read_synced_optional(dir, name)?
        .ok_or_else(|| Error::spec(format!("{}: 同期済みの {name} がありません", dir.display())))
}

/// Executes one deletion in tombstone → vault → source order.
pub fn execute(plan: &DeletePlan) -> Result<()> {
    execute_with_hook(plan, |_| Ok(()))
}

/// Executes with a post-stage hook used to prove crash-safe restart behavior.
#[doc(hidden)]
pub fn execute_with_hook(
    plan: &DeletePlan,
    mut after_stage: impl FnMut(DeleteStage) -> Result<()>,
) -> Result<()> {
    append_tombstone(plan)?;
    after_stage(DeleteStage::Tombstone)?;
    remove_vault(plan)?;
    after_stage(DeleteStage::Vault)?;
    remove_source(plan)?;
    after_stage(DeleteStage::Source)?;
    Ok(())
}

fn append_tombstone(plan: &DeletePlan) -> Result<()> {
    if plan.already_tombstoned
        || contains(&plan.vault_root, &plan.tombstone.repo_id, &plan.run_uid)?
    {
        return Ok(());
    }
    let path = tombstone_path(&plan.vault_root, &plan.tombstone.repo_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let mut line = serde_json::to_vec(&plan.tombstone)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;
    file.write_all(&line).map_err(|e| Error::io(&path, e))?;
    file.sync_data().map_err(|e| Error::io(&path, e))
}

fn remove_vault(plan: &DeletePlan) -> Result<()> {
    if let Some(dir) = &plan.vault_dir
        && dir.exists()
    {
        std::fs::remove_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let slug_dir = plan
        .vault_root
        .join(&plan.tombstone.repo_id)
        .join(&plan.experiment)
        .join("by-slug")
        .join(&plan.run_slug);
    let link = slug_dir.join(&plan.run_uid);
    match std::fs::remove_file(&link) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(&link, e)),
    }
    if slug_dir.is_dir()
        && std::fs::read_dir(&slug_dir)
            .map_err(|e| Error::io(&slug_dir, e))?
            .next()
            .is_none()
    {
        std::fs::remove_dir(&slug_dir).map_err(|e| Error::io(&slug_dir, e))?;
    }
    Ok(())
}

fn remove_source(plan: &DeletePlan) -> Result<()> {
    if let Some(dir) = &plan.source_dir
        && dir.exists()
    {
        std::fs::remove_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    Ok(())
}
