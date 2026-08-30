//! `run.json` — the run's metadata.
//!
//! The shape is frozen by `schema/v1/run.json`; these types only mirror it.

use serde::{Deserialize, Serialize};

/// The schema version every file in a run directory carries.
pub const SCHEMA_VERSION: &str = "1.0";

macro_rules! schema_type {
    ($(#[$meta:meta])* pub struct $name:ident { $($body:tt)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($body)* }
    };
}

/// Where the record came from. Written explicitly so a reader never guesses a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Produced by experiment code, so `code` is required.
    Code,
    /// Entered by hand (annotation, a measurement taken outside any program).
    Manual,
    /// Imported from another tool.
    External,
}

/// Whether the run may leave the machine it was produced on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Default. `runvault sync` skips it unless told otherwise.
    Internal,
    /// May be synced to the aggregation repository without a further flag.
    Public,
}

/// The hash function used for a digest, carried next to the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    /// BLAKE3, what `runvault` writes.
    Blake3,
    /// SHA-256, accepted when a digest comes from elsewhere.
    Sha256,
}

impl Algorithm {
    fn as_str(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
        }
    }
}

/// What a data hash covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HashScope {
    /// A single file's bytes.
    File,
    /// The path-ordered list of `(relative path, content hash)` under a directory.
    DirManifest,
}

impl HashScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::DirManifest => "dir_manifest",
        }
    }
}

schema_type! {
    /// A digest and the function that produced it.
    pub struct Hash {
        /// Which hash function.
        pub algorithm: Algorithm,
        /// 64 lowercase hex characters.
        pub value: String,
    }
}

/// Which dependency manager wrote a lock file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum LockKind {
    /// `Cargo.lock`.
    Cargo,
    /// `uv.lock`.
    Uv,
    /// `poetry.lock`.
    Poetry,
    /// A pip requirements lock.
    Pip,
    /// Anything else.
    Other,
}

impl LockKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Uv => "uv",
            Self::Poetry => "poetry",
            Self::Pip => "pip",
            Self::Other => "other",
        }
    }
}

schema_type! {
    /// A lock file copied into `lock/`, with the digest of the original.
    pub struct Lock {
        /// Which dependency manager wrote it.
        pub kind: LockKind,
        /// Digest of the file's bytes.
        pub hash: Hash,
        /// Where the copy lives, relative to the run directory (`lock/Cargo.lock`).
        pub file: String,
    }
}

schema_type! {
    /// The code that produced the run. Required when `origin` is `code`.
    pub struct Code {
        /// `origin` URL, when the checkout has one.
        pub git_remote: Option<String>,
        /// Branch name at the time of the run.
        pub git_branch: Option<String>,
        /// Full 40-character commit id.
        pub git_commit: String,
        /// Whether the working tree differed from `HEAD`.
        pub git_dirty: bool,
        /// Digest of the working-tree diff. Required when `git_dirty` is true, because a
        /// boolean alone lets two different working trees share an `execution_hash`.
        pub dirty_hash: Option<Hash>,
        /// Where in the repository the run was started from.
        pub repo_relpath: Option<String>,
        /// Lock files bundled under `lock/`.
        pub locks: Vec<Lock>,
    }
}

schema_type! {
    /// The machine and toolchain. `host` is deliberately outside `env_hash`.
    pub struct Env {
        /// BLAKE3 over os / arch / toolchain versions / locks (design note §3.3).
        pub env_hash: String,
        /// Machine name. Recorded, but not part of `env_hash`.
        pub host: String,
        /// Operating system name.
        pub os: String,
        /// CPU architecture.
        pub arch: String,
        /// `rustc --version`, when Rust produced the run.
        pub rustc_version: Option<String>,
        /// Python interpreter version, when Python produced the run.
        pub python_version: Option<String>,
    }
}

schema_type! {
    /// Random-number state. `master_seed` is required for `domain = simulation`.
    pub struct Rng {
        /// The seed every other stream is derived from.
        pub master_seed: Option<u64>,
        /// Which repeat of the same condition this run is.
        pub replicate_index: Option<u64>,
    }
}

schema_type! {
    /// The model under test. Required for `domain = llm-safety`.
    pub struct Llm {
        /// Vendor or gateway.
        pub provider: String,
        /// The dated snapshot id, not the moving alias.
        pub model_snapshot: String,
        /// Sampling temperature.
        pub temperature: Option<f64>,
        /// Digest of the system prompt, so the prompt itself need not be published.
        pub system_prompt_hash: Option<Hash>,
    }
}

schema_type! {
    /// One dataset the run used. `(role, name)` is unique within a run.
    pub struct Dataset {
        /// What the data was used for (`train` / `eval` / `init` / `prompts` / ...).
        pub role: String,
        /// Short name of the dataset.
        pub name: String,
        /// A stable id that includes the version.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub dataset_id: Option<String>,
        /// Version, when it is not already part of `dataset_id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub version: Option<String>,
        /// Digest of the data.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub hash: Option<Hash>,
        /// What the digest covers. Always present when `hash` is.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub hash_scope: Option<HashScope>,
        /// Number of records.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub n: Option<u64>,
        /// Where the data came from.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub uri: Option<String>,
        /// Which split was used. Changing only the split is a change of condition.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub split: Option<String>,
    }
}

schema_type! {
    /// How this run relates to other runs. Four relations, four fields.
    #[derive(Default)]
    pub struct Lineage {
        /// The sweep this run belongs to.
        pub sweep_id: Option<String>,
        /// The sweep's parent run. Only meaningful together with `sweep_id`.
        pub parent_run_uid: Option<String>,
        /// The interrupted run this one continues.
        pub resumed_from: Option<String>,
        /// The run whose records this one recomputed.
        pub derived_from: Option<String>,
    }
}

/// What part of a paper a run reproduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    /// A table.
    Table,
    /// A figure.
    Figure,
    /// A section.
    Section,
    /// A claim in the prose.
    Claim,
}

schema_type! {
    /// One target experiment inside the paper.
    pub struct Target {
        /// Short id used by `reference.csv`.
        pub target_id: String,
        /// Table / figure / section / claim.
        pub kind: TargetKind,
        /// Human label, e.g. `Table 3`.
        pub label: String,
        /// Panel within a figure.
        pub panel: Option<String>,
        /// Row within a table.
        pub row: Option<String>,
        /// The condition the row stands for.
        pub condition: Option<String>,
    }
}

schema_type! {
    /// The paper being reproduced.
    pub struct Work {
        /// Normalized id: `doi:…` / `arxiv:…` / `paperid:…`.
        pub work_id: String,
        /// DOI, when there is one.
        pub doi: Option<String>,
        /// arXiv id, when there is one.
        pub arxiv_id: Option<String>,
        /// Vault paper id (`P00000009`).
        pub paper_id: Option<String>,
        /// Title, as redundancy against a mistyped id.
        pub title: String,
        /// Publication year.
        pub year: Option<i64>,
        /// Which version the numbers were read from (`arXiv v2` / `published`).
        pub source_version: Option<String>,
    }
}

schema_type! {
    /// The authors' own implementation, when it was consulted.
    pub struct UpstreamImpl {
        /// Repository URL.
        pub url: String,
        /// The commit that was read.
        pub commit: Option<String>,
    }
}

schema_type! {
    /// What research the run belongs to.
    pub struct Research {
        /// Whether the run reproduces a published result.
        pub is_replication: bool,
        /// The paper. Required when `is_replication` is true.
        pub work: Option<Work>,
        /// The targets inside the paper. At least one when `is_replication` is true.
        pub targets: Vec<Target>,
        /// The authors' implementation, or `null` to record that only the paper was used.
        pub upstream_impl: Option<UpstreamImpl>,
        /// Back-link to the replication note in the Obsidian vault.
        pub obsidian_note: Option<String>,
        /// JIRA issues this run belongs to.
        pub jira: Vec<String>,
    }
}

schema_type! {
    /// `run.json`.
    pub struct RunMeta {
        /// Version of `schema/v1`.
        pub schema_version: String,
        /// Version of `vocabulary.toml` the run was written against.
        pub vocab_version: String,
        /// Version of the `runvault` implementation that wrote the run.
        pub runvault_version: String,
        /// ULID. The primary key of every file and index.
        pub run_uid: String,
        /// Directory name. Readable, but not unique.
        pub run_slug: String,
        /// Stable id of the repository, given by the experiment.
        pub repo_id: String,
        /// The experiment this run belongs to.
        pub experiment: String,
        /// Which subcommand produced it.
        pub subcommand: String,
        /// Field of study (`simulation` / `llm-safety` / `anomaly-detection` / ...).
        pub domain: String,
        /// Hash of the experimental condition.
        pub config_hash: String,
        /// Hash of condition + seed + code + environment.
        pub execution_hash: String,
        /// When the run started.
        pub created_at: String,
        /// How the program was invoked; `config.json` is already resolved.
        pub cli_args: Vec<String>,
        /// Where the record came from.
        pub origin: Origin,
        /// Whether the run may be synced.
        pub visibility: Visibility,
        /// The code. `null` when `origin` is not `code`.
        pub code: Option<Code>,
        /// The machine and toolchain. Required whatever the origin.
        pub env: Env,
        /// Random-number state.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub rng: Option<Rng>,
        /// The model under test.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub llm: Option<Llm>,
        /// Datasets used. An empty array means "none", which differs from "not recorded".
        pub data: Vec<Dataset>,
        /// Relations to other runs.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub lineage: Option<Lineage>,
        /// Which research the run belongs to.
        pub research: Research,
        /// Field-specific metadata, so the top level never grows a block per field.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub ext: Option<serde_json::Map<String, serde_json::Value>>,
    }
}

impl Hash {
    /// A BLAKE3 digest.
    pub fn blake3(value: impl Into<String>) -> Self {
        Self {
            algorithm: Algorithm::Blake3,
            value: value.into(),
        }
    }

    pub(crate) fn algorithm_str(&self) -> &'static str {
        self.algorithm.as_str()
    }
}

impl Dataset {
    pub(crate) fn hash_scope_str(&self) -> &'static str {
        self.hash_scope.map(HashScope::as_str).unwrap_or("")
    }
}

impl Lock {
    pub(crate) fn kind_str(&self) -> &'static str {
        self.kind.as_str()
    }
}

// --- builders -------------------------------------------------------------

impl Dataset {
    /// A dataset used in the given role.
    pub fn new(role: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            name: name.into(),
            dataset_id: None,
            version: None,
            hash: None,
            hash_scope: None,
            n: None,
            uri: None,
            split: None,
        }
    }

    /// Training data.
    pub fn train(name: impl Into<String>) -> Self {
        Self::new("train", name)
    }

    /// Evaluation data.
    pub fn eval(name: impl Into<String>) -> Self {
        Self::new("eval", name)
    }

    /// The initial state a simulation starts from.
    pub fn init(name: impl Into<String>) -> Self {
        Self::new("init", name)
    }

    /// The prompt set an attack draws from.
    pub fn prompts(name: impl Into<String>) -> Self {
        Self::new("prompts", name)
    }

    /// A stable id that includes the version.
    pub fn dataset_id(mut self, id: impl Into<String>) -> Self {
        self.dataset_id = Some(id.into());
        self
    }

    /// The version, when it is not already inside `dataset_id`.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Which split was used.
    pub fn split(mut self, split: impl Into<String>) -> Self {
        self.split = Some(split.into());
        self
    }

    /// How many records.
    pub fn n(mut self, n: u64) -> Self {
        self.n = Some(n);
        self
    }

    /// Where the data came from.
    pub fn uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// Hashes a single file's bytes.
    pub fn hash_of_file(mut self, path: impl AsRef<std::path::Path>) -> crate::Result<Self> {
        let (digest, _) = crate::files::digest_file(path.as_ref())?;
        self.hash = Some(Hash::blake3(digest));
        self.hash_scope = Some(HashScope::File);
        Ok(self)
    }

    /// Hashes a directory as the path-ordered list of `(relative path, content hash)`.
    ///
    /// Timestamps and permissions are left out: the same bytes laid out the same
    /// way must hash the same on another machine.
    pub fn hash_of_dir(mut self, path: impl AsRef<std::path::Path>) -> crate::Result<Self> {
        let root = path.as_ref();
        let mut blob = Vec::new();
        for rel in crate::files::walk_files(root, root)? {
            let (digest, _) = crate::files::digest_file(&root.join(&rel))?;
            crate::canonical::push_lp(&mut blob, rel.as_bytes());
            crate::canonical::push_lp(&mut blob, digest.as_bytes());
        }
        self.hash = Some(Hash::blake3(crate::canonical::blake3_hex(&blob)));
        self.hash_scope = Some(HashScope::DirManifest);
        Ok(self)
    }
}

impl Work {
    fn with_id(work_id: String) -> Self {
        Self {
            work_id,
            doi: None,
            arxiv_id: None,
            paper_id: None,
            title: String::new(),
            year: None,
            source_version: None,
        }
    }

    /// A paper identified by DOI.
    pub fn doi(doi: impl Into<String>) -> Self {
        let doi = doi.into();
        let mut work = Self::with_id(format!("doi:{doi}"));
        work.doi = Some(doi);
        work
    }

    /// A paper identified by arXiv id.
    pub fn arxiv(id: impl Into<String>) -> Self {
        let id = id.into();
        let mut work = Self::with_id(format!("arxiv:{id}"));
        work.arxiv_id = Some(id);
        work
    }

    /// A paper identified by its id in the vault (`P00000009`).
    pub fn paper_id(id: impl Into<String>) -> Self {
        let id = id.into();
        let mut work = Self::with_id(format!("paperid:{id}"));
        work.paper_id = Some(id);
        work
    }

    /// The title, kept as redundancy against a mistyped id.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Publication year.
    pub fn year(mut self, year: i64) -> Self {
        self.year = Some(year);
        self
    }

    /// Which version the numbers were read from. The same table differs between versions.
    pub fn source_version(mut self, version: impl Into<String>) -> Self {
        self.source_version = Some(version.into());
        self
    }

    /// Starts a replication that reproduces this target.
    pub fn target(self, target: Target) -> Replication {
        Replication::new(self).target(target)
    }
}

impl Target {
    fn new(kind: TargetKind, target_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            target_id: target_id.into(),
            kind,
            label: label.into(),
            panel: None,
            row: None,
            condition: None,
        }
    }

    /// A table in the paper.
    pub fn table(target_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(TargetKind::Table, target_id, label)
    }

    /// A figure in the paper.
    pub fn figure(target_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(TargetKind::Figure, target_id, label)
    }

    /// A section of the paper.
    pub fn section(target_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(TargetKind::Section, target_id, label)
    }

    /// A claim made in the prose.
    pub fn claim(target_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(TargetKind::Claim, target_id, label)
    }

    /// Which panel of the figure.
    pub fn panel(mut self, panel: impl Into<String>) -> Self {
        self.panel = Some(panel.into());
        self
    }

    /// Which row of the table.
    pub fn row(mut self, row: impl Into<String>) -> Self {
        self.row = Some(row.into());
        self
    }

    /// The condition the row or panel stands for.
    pub fn condition(mut self, condition: impl Into<String>) -> Self {
        self.condition = Some(condition.into());
        self
    }
}

/// A replication under construction: the paper, its targets and the back-links.
#[derive(Debug, Clone, PartialEq)]
pub struct Replication {
    work: Work,
    targets: Vec<Target>,
    upstream_impl: Option<UpstreamImpl>,
    obsidian_note: Option<String>,
    jira: Vec<String>,
}

impl Replication {
    /// Starts from the paper.
    pub fn new(work: Work) -> Self {
        Self {
            work,
            targets: Vec::new(),
            upstream_impl: None,
            obsidian_note: None,
            jira: Vec::new(),
        }
    }

    /// Adds a target experiment. One run may reproduce several.
    pub fn target(mut self, target: Target) -> Self {
        self.targets.push(target);
        self
    }

    /// The authors' own implementation, when it was consulted.
    pub fn upstream_impl(mut self, url: impl Into<String>, commit: Option<String>) -> Self {
        self.upstream_impl = Some(UpstreamImpl {
            url: url.into(),
            commit,
        });
        self
    }

    /// Back-link to the replication note.
    pub fn obsidian_note(mut self, note: impl Into<String>) -> Self {
        self.obsidian_note = Some(note.into());
        self
    }

    /// The JIRA issue this work belongs to.
    pub fn jira(mut self, issue: impl Into<String>) -> Self {
        self.jira.push(issue.into());
        self
    }
}

impl From<Work> for Replication {
    fn from(work: Work) -> Self {
        Self::new(work)
    }
}

impl From<Replication> for Research {
    fn from(r: Replication) -> Self {
        Self {
            is_replication: true,
            work: Some(r.work),
            targets: r.targets,
            upstream_impl: r.upstream_impl,
            obsidian_note: r.obsidian_note,
            jira: r.jira,
        }
    }
}

impl Default for Research {
    /// A run that is not reproducing anything.
    fn default() -> Self {
        Self {
            is_replication: false,
            work: None,
            targets: Vec::new(),
            upstream_impl: None,
            obsidian_note: None,
            jira: Vec::new(),
        }
    }
}
