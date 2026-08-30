//! `runvault sync` — the copy that keeps the record from living on one machine.
//!
//! A replication repository's `.gitignore` excludes `results/`, so nothing but
//! this copies the record anywhere else (design note §1.4). What is copied is
//! the light half — the files that reconstruct the condition, the result, the
//! environment and the provenance — and never `artifacts/`, whose identity
//! `manifest.csv` already carries.
//!
//! Two rules shape everything here. The destination must *declare* that it is
//! private, and a missing declaration stops the sync rather than defaulting to
//! permissive: the question being answered is whether a prompt, a capture or a
//! fragment of internal data is about to enter a git history it cannot leave.
//! And the copy is a copy: the source run directory is never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::config::ConfigEnvelope;
use crate::error::{Error, Result};
use crate::meta::{Algorithm, Hash, RunMeta, SCHEMA_VERSION, Visibility};
use crate::{env, files, legacy, verify};

/// The declaration an aggregation repository must carry at its root.
pub const VAULT_CONFIG: &str = "runvault-vault.toml";

/// The receipt left in every destination run directory.
pub const RECEIPT: &str = "sync.json";

/// The files a canonical run contributes, before `lock/` and the globs.
const CANONICAL_FILES: [&str; 7] = [
    "run.json",
    "config.json",
    "status.json",
    "metrics.csv",
    "reference.csv",
    "manifest.csv",
    "events.jsonl",
];

/// What a legacy run contributes: the small text files a reader can still use.
///
/// Everything else in a legacy directory is a figure, a checkpoint or a pickle.
/// Those stay where they are, exactly as `artifacts/` does for a canonical run.
const LEGACY_EXTENSIONS: [&str; 9] = [
    "json", "jsonl", "csv", "tsv", "txt", "md", "yaml", "yml", "toml",
];

/// The directories §1.4 keeps out of the aggregation repository.
///
/// The rule is about what the file *is*, not what it is named: a per-step grid
/// dump is the heavy half of the record whether it is written as `.npy` or as
/// `.csv`. Deciding by extension alone let `snapshots/` through, which is the
/// one thing §1.4 names outright — a single repository shipped 745 files where
/// it should have shipped 30.
const NEVER_SYNCED_DIRS: [&str; 4] = ["artifacts", "logs", "snapshots", "figures"];

/// Whether any component of a relative path is a directory that never travels.
fn is_heavy_half(rel: &str) -> bool {
    Path::new(rel)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .any(|part| NEVER_SYNCED_DIRS.contains(&part))
}

/// Files that only ever grow, and whose history a re-sync must not rewrite.
fn is_append_only(rel: &str) -> bool {
    matches!(rel, "events.jsonl" | "metrics.csv")
}

/// Files that identify the run, and so cannot differ between two syncs of it.
fn is_immutable(rel: &str) -> bool {
    matches!(rel, "run.json" | "config.json")
}

// ---------------------------------------------------------------------------
// The declaration that authorizes writing
// ---------------------------------------------------------------------------

fn default_compress_over_mib() -> f64 {
    10.0
}

/// `runvault-vault.toml`, as `schema/v1/vault.config.json` defines it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// Version of `schema/v1`.
    pub schema_version: String,
    /// Only `private` is a legal value. A public destination is never written to.
    pub visibility: String,
    /// Above this size a file is stored compressed.
    #[serde(default = "default_compress_over_mib")]
    pub compress_over_mib: f64,
    /// Whether `visibility = internal` runs are accepted without a flag.
    #[serde(default)]
    pub allow_internal: bool,
    /// Free text, for whoever finds the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl VaultConfig {
    /// Rejects anything but a private destination.
    ///
    /// An unknown key is an error rather than a warning: a warning leaves a
    /// misspelled `visibility` in place, and the sync keeps running against a
    /// destination nobody declared private.
    fn validate(&self, path: &Path) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::spec(format!(
                "{}: schema_version は \"{SCHEMA_VERSION}\" である必要があります ( 実際は \"{}\")",
                path.display(),
                self.schema_version
            )));
        }
        if self.visibility != "private" {
            return Err(Error::spec(format!(
                "{}: visibility が \"{}\" です．private 以外の集約先へは同期しません",
                path.display(),
                self.visibility
            )));
        }
        // `NaN` is neither above nor below the threshold, and a destination
        // that cannot say when to compress has not said anything.
        if !self.compress_over_mib.is_finite() || self.compress_over_mib <= 0.0 {
            return Err(Error::spec(format!(
                "{}: compress_over_mib は正の数である必要があります",
                path.display()
            )));
        }
        Ok(())
    }

    /// The threshold in bytes.
    pub fn compress_over_bytes(&self) -> u64 {
        (self.compress_over_mib * 1024.0 * 1024.0) as u64
    }
}

/// Finds the declaration that authorizes writing into `dest`, or refuses.
///
/// The search climbs from the destination and takes the first file it finds.
/// A symbolic link is not followed: a link is something a different repository
/// can point at a permissive declaration, and the whole purpose of the file is
/// that the destination itself said what it is.
pub fn load_vault_config(dest: &Path) -> Result<(PathBuf, VaultConfig)> {
    for dir in dest.ancestors() {
        let path = dir.join(VAULT_CONFIG);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            return Err(Error::spec(format!(
                "{} がシンボリックリンクです (辿らずに停止します)",
                path.display()
            )));
        }
        if !meta.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let config: VaultConfig = toml::from_str(&text).map_err(|e| {
            Error::spec(format!(
                "{}: {VAULT_CONFIG} を読めません: {e}",
                path.display()
            ))
        })?;
        config.validate(&path)?;
        return Ok((dir.to_path_buf(), config));
    }
    Err(Error::spec(format!(
        "{} から親を遡っても {VAULT_CONFIG} が見つかりません (宣言の無い場所へは同期しません)",
        dest.display()
    )))
}

// ---------------------------------------------------------------------------
// The receipt
// ---------------------------------------------------------------------------

/// How a file is stored at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Stored byte for byte.
    None,
    /// Stored as `<name>.zst`.
    Zstd,
}

/// A digest and a size, for one end of a copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    /// Digest of the bytes at this end.
    pub hash: Hash,
    /// Size of the bytes at this end.
    pub bytes: u64,
}

/// One file as the receipt records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SyncedFile {
    /// Relative path at the source.
    pub path: String,
    /// Relative path at the destination; `.zst` when compressed.
    pub stored_path: String,
    /// Whether the destination copy is compressed.
    pub compression: Compression,
    /// The bytes as they were read.
    pub source: FileDigest,
    /// The bytes as they were written.
    pub stored: FileDigest,
}

/// Where a copy came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SyncSource {
    /// The machine that held the record.
    pub host: String,
    /// The repository the run belongs to.
    pub repo_id: String,
    /// The source run directory.
    pub path: String,
}

/// `sync.json` — what was sent, when, and in what form.
///
/// This is the only place the destination learns a legacy run's `run_key`: the
/// key is built from the path *below the source `results/`*, which the
/// destination layout no longer shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SyncReceipt {
    /// Version of `schema/v1`.
    pub schema_version: String,
    /// `null` for a legacy run, which has none.
    pub run_uid: Option<String>,
    /// The index key. `run_uid`, or `legacy:<repo_id>:<relative path>`.
    pub run_key: String,
    /// How many times this run has been synced, counting this one.
    pub generation: u64,
    /// When this sync happened.
    pub synced_at: String,
    /// Whether `verify --deep` passed. Absent for a legacy run, which has no
    /// invariants to check — absent means unexamined, not failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Where the copy came from.
    pub source: SyncSource,
    /// Every file placed at the destination.
    pub files: Vec<SyncedFile>,
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// One file the sync would place, before anything is written.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedFile {
    /// Relative path at the source.
    pub path: String,
    /// Relative path at the destination.
    pub stored_path: String,
    /// Whether it would be compressed.
    pub compression: Compression,
    /// Size at the source, so `--dry-run` can say what enters the repository.
    pub bytes: u64,
}

/// A run the sync would send, with everything decided but nothing written.
#[derive(Debug, Clone)]
pub struct SyncPlan {
    /// The source run directory.
    pub run_dir: PathBuf,
    /// The destination run directory.
    pub dest: PathBuf,
    /// The index key.
    pub run_key: String,
    /// `None` for a legacy run.
    pub run_uid: Option<String>,
    /// `None` for a legacy run: the `by-slug` link is a canonical convenience.
    pub run_slug: Option<String>,
    /// The repository the run belongs to.
    pub repo_id: String,
    /// Whether `verify --deep` was run and passed.
    pub verified: Option<bool>,
    /// The files, in destination order.
    pub files: Vec<PlannedFile>,
}

impl SyncPlan {
    /// Total size at the source, which is what `--dry-run` reports.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

/// What planning decided about one run directory.
#[derive(Debug, Clone)]
pub enum Planned {
    /// The run would be sent.
    Send(Box<SyncPlan>),
    /// The run would not be sent, and why. Never silently dropped.
    Skipped {
        /// The run directory that stays where it is.
        run_dir: PathBuf,
        /// Why, in the words the report prints.
        reason: String,
    },
}

/// What the caller decided, as opposed to what the destination declared.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// Accept runs that did not declare themselves public.
    pub allow_internal: bool,
    /// Above this size a file is stored compressed.
    pub compress_over_bytes: u64,
}

/// Plans every run under `results_root`, canonical and legacy alike.
///
/// The two kinds are found by different scanners and cannot overlap: a
/// directory holding `run.json` belongs to one and is refused by the other.
pub fn plan_all(
    results_root: &Path,
    repo_id: &str,
    vault_root: &Path,
    options: &SyncOptions,
) -> Result<Vec<Planned>> {
    let mut out = Vec::new();
    for dir in crate::paths::run_dirs(results_root)? {
        if dir.join("run.json").is_file() {
            out.push(plan_canonical(&dir, repo_id, vault_root, options)?);
        }
    }
    for dir in legacy::find_run_dirs(results_root)? {
        out.push(plan_legacy(
            results_root,
            &dir,
            repo_id,
            vault_root,
            options,
        )?);
    }
    out.sort_by_key(|planned| match planned {
        Planned::Send(plan) => plan.run_dir.clone(),
        Planned::Skipped { run_dir, .. } => run_dir.clone(),
    });
    Ok(out)
}

/// Plans one canonical run.
pub fn plan_canonical(
    run_dir: &Path,
    repo_id: &str,
    vault_root: &Path,
    options: &SyncOptions,
) -> Result<Planned> {
    let skip = |reason: String| {
        Ok(Planned::Skipped {
            run_dir: run_dir.to_path_buf(),
            reason,
        })
    };

    let meta: RunMeta = files::read_json(&run_dir.join("run.json"))?;
    if meta.repo_id != repo_id {
        return Err(Error::spec(format!(
            "{}: run.json の repo_id `{}` が指定された `{repo_id}` と違います",
            run_dir.display(),
            meta.repo_id
        )));
    }
    if meta.visibility == Visibility::Internal && !options.allow_internal {
        return skip(
            "visibility = internal (--allow-internal か集約先の allow_internal が要ります)".into(),
        );
    }
    // A run that contradicts itself is not preserved, it is spread: the
    // aggregation layer would then hold a record no one can trust.
    if let Err(e) = verify::deep(run_dir) {
        return skip(format!("verify --deep に通りません: {e}"));
    }

    let files = canonical_files(run_dir, options.compress_over_bytes)?;
    Ok(Planned::Send(Box::new(SyncPlan {
        run_dir: run_dir.to_path_buf(),
        dest: vault_root
            .join(&meta.repo_id)
            .join(&meta.experiment)
            .join(&meta.run_uid),
        run_key: meta.run_uid.clone(),
        run_uid: Some(meta.run_uid.clone()),
        run_slug: Some(meta.run_slug.clone()),
        repo_id: meta.repo_id.clone(),
        verified: Some(true),
        files,
    })))
}

/// Plans one legacy run.
///
/// It keeps its place in the tree rather than being re-filed under an id it
/// does not have, and its key travels in the receipt.
pub fn plan_legacy(
    results_root: &Path,
    run_dir: &Path,
    repo_id: &str,
    vault_root: &Path,
    options: &SyncOptions,
) -> Result<Planned> {
    let relative = run_dir.strip_prefix(results_root).map_err(|_| {
        Error::spec(format!(
            "{} は {} の下にありません",
            run_dir.display(),
            results_root.display()
        ))
    })?;
    let relpath = legacy::normalize_relpath(relative)?;

    if !options.allow_internal {
        return Ok(Planned::Skipped {
            run_dir: run_dir.to_path_buf(),
            reason: "legacy run は visibility を宣言していません (--allow-internal が要ります)"
                .into(),
        });
    }

    let mut files = Vec::new();
    for rel in files::walk_files(run_dir, run_dir)? {
        let readable = Path::new(&rel)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| LEGACY_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
        if readable && !is_heavy_half(&rel) {
            files.push(planned_file(run_dir, rel, options.compress_over_bytes)?);
        }
    }

    Ok(Planned::Send(Box::new(SyncPlan {
        run_dir: run_dir.to_path_buf(),
        dest: vault_root.join(repo_id).join(&relpath),
        run_key: legacy::run_key(repo_id, &relpath),
        run_uid: None,
        run_slug: None,
        repo_id: repo_id.to_string(),
        verified: None,
        files,
    })))
}

/// The default set, `lock/`, and whatever the run's own globs add or remove.
fn canonical_files(run_dir: &Path, compress_over_bytes: u64) -> Result<Vec<PlannedFile>> {
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    for name in CANONICAL_FILES {
        if run_dir.join(name).is_file() {
            chosen.insert(name.to_string());
        }
    }
    for rel in files::walk_files(&run_dir.join("lock"), run_dir)? {
        chosen.insert(rel);
    }

    let config: ConfigEnvelope = files::read_json(&run_dir.join("config.json"))?;
    if !config.runvault.sync_include.is_empty() {
        let include = globs(&config.runvault.sync_include, "sync_include")?;
        for rel in files::walk_files(run_dir, run_dir)? {
            if include.is_match(&rel) {
                chosen.insert(rel);
            }
        }
    }
    if !config.runvault.sync_exclude.is_empty() {
        // Exclusion wins wherever the two meet: between sending something that
        // should not leave and holding something back, the record errs on
        // holding back.
        let exclude = globs(&config.runvault.sync_exclude, "sync_exclude")?;
        chosen.retain(|rel| !exclude.is_match(rel));
    }

    // Without `run.json` the destination holds files that name no run, and the
    // receipt would be the only thing saying what they are.
    for required in ["run.json", "config.json"] {
        if !chosen.contains(required) && run_dir.join(required).is_file() {
            return Err(Error::spec(format!(
                "sync_exclude が {required} を除外しています (run を同定できなくなります)"
            )));
        }
    }

    chosen
        .into_iter()
        .map(|rel| planned_file(run_dir, rel, compress_over_bytes))
        .collect()
}

fn globs(patterns: &[String], field: &str) -> Result<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        let glob = globset::Glob::new(pattern)
            .map_err(|e| Error::spec(format!("{field} の glob `{pattern}` が不正です: {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| Error::spec(format!("{field} の glob を組み立てられません: {e}")))
}

fn planned_file(run_dir: &Path, rel: String, compress_over_bytes: u64) -> Result<PlannedFile> {
    let path = run_dir.join(&rel);
    let bytes = std::fs::metadata(&path)
        .map_err(|e| Error::io(&path, e))?
        .len();
    let compression = if bytes > compress_over_bytes {
        Compression::Zstd
    } else {
        Compression::None
    };
    let stored_path = match compression {
        Compression::None => rel.clone(),
        Compression::Zstd => format!("{rel}.zst"),
    };
    Ok(PlannedFile {
        path: rel,
        stored_path,
        compression,
        bytes,
    })
}

// ---------------------------------------------------------------------------
// Copying
// ---------------------------------------------------------------------------

/// What one run's sync produced.
#[derive(Debug, Clone)]
pub struct Synced {
    /// The receipt written at the destination.
    pub receipt: SyncReceipt,
    /// Files an earlier generation placed that this one did not send again.
    /// Left where they are, and reported so nobody has to discover them.
    pub left_behind: Vec<String>,
}

/// Carries out a plan and writes the receipt.
///
/// Nothing here writes into the source run directory: the aggregation copy is a
/// copy, and a run that was synced looks exactly like one that was not.
pub fn execute(plan: &SyncPlan) -> Result<Synced> {
    let previous = read_receipt(&plan.dest)?;
    std::fs::create_dir_all(&plan.dest).map_err(|e| Error::io(&plan.dest, e))?;

    let mut written = Vec::new();
    for file in &plan.files {
        let source = plan.run_dir.join(&file.path);
        let stored = plan.dest.join(&file.stored_path);

        if let Some(previous) = previous_form(previous.as_ref(), &file.path) {
            let existing = plan.dest.join(&previous.stored_path);
            if existing.is_file() {
                guard_rewrite(&file.path, &existing, previous.compression, &source)?;
            }
        }

        if let Some(parent) = stored.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let source_digest = files::digest_file(&source)?;
        copy_into(&source, &stored, file.compression)?;
        let stored_digest = files::digest_file(&stored)?;

        // Both forms present would leave a reader to decide which is canonical.
        remove_other_form(&plan.dest, file)?;

        written.push(SyncedFile {
            path: file.path.clone(),
            stored_path: file.stored_path.clone(),
            compression: file.compression,
            source: digest(source_digest),
            stored: digest(stored_digest),
        });
    }

    let left_behind = left_behind(&plan.dest, previous.as_ref(), &written);

    let receipt = SyncReceipt {
        schema_version: SCHEMA_VERSION.to_string(),
        run_uid: plan.run_uid.clone(),
        run_key: plan.run_key.clone(),
        generation: previous.as_ref().map_or(0, |r| r.generation) + 1,
        synced_at: Local::now().to_rfc3339(),
        verified: plan.verified,
        source: SyncSource {
            host: env::host(),
            repo_id: plan.repo_id.clone(),
            path: std::fs::canonicalize(&plan.run_dir)
                .unwrap_or_else(|_| plan.run_dir.clone())
                .to_string_lossy()
                .to_string(),
        },
        files: written,
    };
    files::write_json_atomically(&plan.dest.join(RECEIPT), &receipt)?;

    link_by_slug(plan)?;
    Ok(Synced {
        receipt,
        left_behind,
    })
}

fn digest((value, bytes): (String, u64)) -> FileDigest {
    FileDigest {
        hash: Hash {
            algorithm: Algorithm::Blake3,
            value,
        },
        bytes,
    }
}

/// The receipt already at the destination, if this run has been sent before.
pub fn read_receipt(dest: &Path) -> Result<Option<SyncReceipt>> {
    let path = dest.join(RECEIPT);
    if !path.is_file() {
        return Ok(None);
    }
    files::read_json(&path).map(Some)
}

fn previous_form<'a>(previous: Option<&'a SyncReceipt>, path: &str) -> Option<&'a SyncedFile> {
    previous?.files.iter().find(|f| f.path == path)
}

/// Refuses a re-sync that would rewrite what the destination already holds.
///
/// An append-only file may only grow, and the check is that the stored bytes
/// are a *prefix* of the new ones. Comparing sizes is not enough: a file whose
/// past rows were edited is the same length, or longer, and would pass.
fn guard_rewrite(
    rel: &str,
    existing: &Path,
    compression: Compression,
    source: &Path,
) -> Result<()> {
    if is_append_only(rel) {
        if !stored_is_prefix_of(existing, compression, source)? {
            return Err(Error::verify(format!(
                "{rel} は追記のみのはずですが，集約先の内容が同期元の先頭と一致しません (過去の行が書き換わっています)"
            )));
        }
        return Ok(());
    }
    if is_immutable(rel) {
        let stored = read_stored(existing, compression)?;
        let fresh = std::fs::read(source).map_err(|e| Error::io(source, e))?;
        if stored != fresh {
            return Err(Error::verify(format!(
                "{rel} が集約先の内容と違います (同じ run が別の内容を主張しています)"
            )));
        }
    }
    Ok(())
}

/// Whether the destination copy is a prefix of the file about to replace it.
fn stored_is_prefix_of(existing: &Path, compression: Compression, source: &Path) -> Result<bool> {
    let mut stored = open_stored(existing, compression)?;
    let mut fresh =
        std::io::BufReader::new(std::fs::File::open(source).map_err(|e| Error::io(source, e))?);

    let mut left = [0u8; 64 * 1024];
    let mut right = [0u8; 64 * 1024];
    loop {
        let n = read_full(&mut stored, &mut left)?;
        if n == 0 {
            return Ok(true);
        }
        let m = read_full(&mut fresh, &mut right[..n])?;
        if m != n || left[..n] != right[..n] {
            return Ok(false);
        }
    }
}

/// Fills `buffer` unless the reader ends first; returns how much was read.
fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]).map_err(Error::PlainIo)? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn open_stored(path: &Path, compression: Compression) -> Result<Box<dyn Read>> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    Ok(match compression {
        Compression::None => Box::new(std::io::BufReader::new(file)),
        Compression::Zstd => {
            Box::new(zstd::stream::read::Decoder::new(file).map_err(Error::PlainIo)?)
        }
    })
}

fn read_stored(path: &Path, compression: Compression) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    open_stored(path, compression)?
        .read_to_end(&mut out)
        .map_err(Error::PlainIo)?;
    Ok(out)
}

/// Writes `source` to `stored`, compressing on the way when asked.
fn copy_into(source: &Path, stored: &Path, compression: Compression) -> Result<()> {
    let mut reader =
        std::io::BufReader::new(std::fs::File::open(source).map_err(|e| Error::io(source, e))?);
    let temporary = stored.with_extension(format!(
        "{}.part",
        stored.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    {
        let file = std::fs::File::create(&temporary).map_err(|e| Error::io(&temporary, e))?;
        let mut writer = std::io::BufWriter::new(file);
        match compression {
            Compression::None => {
                std::io::copy(&mut reader, &mut writer).map_err(Error::PlainIo)?;
            }
            Compression::Zstd => {
                zstd::stream::copy_encode(&mut reader, &mut writer, 3).map_err(Error::PlainIo)?;
            }
        }
        writer.flush().map_err(Error::PlainIo)?;
    }
    std::fs::rename(&temporary, stored).map_err(|e| Error::io(stored, e))
}

/// Deletes the other spelling of a file that just crossed the size threshold.
fn remove_other_form(dest: &Path, file: &PlannedFile) -> Result<()> {
    let other = match file.compression {
        Compression::None => dest.join(format!("{}.zst", file.path)),
        Compression::Zstd => dest.join(&file.path),
    };
    if other.is_file() {
        std::fs::remove_file(&other).map_err(|e| Error::io(&other, e))?;
    }
    Ok(())
}

/// What an earlier sync placed that this one did not send again.
///
/// These are reported, never deleted. The aggregation copy exists because the
/// source lives on one machine; deleting from it because the source no longer
/// offers a file would remove the last copy of exactly the run that needed one.
/// Withdrawing a file is not something a sync can do anyway — §1.4 is explicit
/// that git does not forget — so the honest outcome is to say what is there.
fn left_behind(dest: &Path, previous: Option<&SyncReceipt>, written: &[SyncedFile]) -> Vec<String> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let kept: BTreeSet<&str> = written.iter().map(|f| f.stored_path.as_str()).collect();
    previous
        .files
        .iter()
        .filter(|file| !kept.contains(file.stored_path.as_str()))
        .filter(|file| dest.join(&file.stored_path).is_file())
        .map(|file| file.stored_path.clone())
        .collect()
}

/// Links `by-slug/<run_slug>/<run_uid>` at the run, one directory per name.
///
/// `run_slug` is readable but not unique, so it names a directory of links
/// rather than a link: pointing `by-slug/<run_slug>` straight at a run would
/// treat a non-unique name as unique again, which is the mistake the `run_uid`
/// layout exists to avoid.
#[cfg(unix)]
fn link_by_slug(plan: &SyncPlan) -> Result<()> {
    let (Some(slug), Some(uid)) = (&plan.run_slug, &plan.run_uid) else {
        return Ok(());
    };
    let Some(experiment) = plan.dest.parent() else {
        return Ok(());
    };
    let dir = experiment.join("by-slug").join(slug);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let link = dir.join(uid);
    if std::fs::symlink_metadata(&link).is_ok() {
        std::fs::remove_file(&link).map_err(|e| Error::io(&link, e))?;
    }
    std::os::unix::fs::symlink(Path::new("../..").join(uid), &link).map_err(|e| Error::io(&link, e))
}

#[cfg(not(unix))]
fn link_by_slug(_plan: &SyncPlan) -> Result<()> {
    Ok(())
}

/// What one `runvault sync` did, for the report the command prints.
#[derive(Debug, Default)]
pub struct SyncSummary {
    /// Runs sent, with their receipts.
    pub sent: Vec<Synced>,
    /// Runs held back, and why.
    pub skipped: BTreeMap<PathBuf, String>,
}

/// Plans and carries out a sync of every run under `results_root`.
pub fn sync_all(
    results_root: &Path,
    repo_id: &str,
    vault_root: &Path,
    allow_internal: bool,
) -> Result<SyncSummary> {
    let (_, config) = load_vault_config(vault_root)?;
    let options = SyncOptions {
        allow_internal: allow_internal || config.allow_internal,
        compress_over_bytes: config.compress_over_bytes(),
    };
    let mut summary = SyncSummary::default();
    for planned in plan_all(results_root, repo_id, vault_root, &options)? {
        match planned {
            Planned::Send(plan) => summary.sent.push(execute(&plan)?),
            Planned::Skipped { run_dir, reason } => {
                summary.skipped.insert(run_dir, reason);
            }
        }
    }
    Ok(summary)
}
