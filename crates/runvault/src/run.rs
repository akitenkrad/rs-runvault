//! `Run` — one execution, one directory, append-only.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::config::{ConfigEnvelope, Exclusions, RunvaultBlock};
use crate::error::{Error, Result};
use crate::files;
use crate::hash::{config_hash, execution_hash};
use crate::ids;
use crate::lockfile::{Heartbeat, LockRecord};
use crate::meta::{
    Code, Dataset, Env, Lineage, Llm, Origin, Replication, Research, RunMeta, SCHEMA_VERSION,
    Visibility,
};
use crate::status::{Counts, RunStatus, State, StatusError};
use crate::verify;
use crate::vocabulary;

const METRICS_HEADER: [&str; 6] = ["run_uid", "step", "step_unit", "scope", "name", "value"];
const REFERENCE_HEADER: [&str; 8] = [
    "run_uid",
    "step",
    "step_unit",
    "scope",
    "name",
    "value",
    "target_id",
    "source",
];
const MANIFEST_HEADER: [&str; 5] = ["run_uid", "path", "algorithm", "digest", "bytes"];

/// The default `scope` of a metric: a single number for the whole run.
const DEFAULT_SCOPE: &str = "run";

/// How many `-N` suffixes to try before giving up on a colliding directory name.
const MAX_COLLISION_INDEX: u64 = 999;

/// Everything a run needs to know before it starts.
#[derive(Debug, Clone)]
pub struct RunOptions {
    experiment: String,
    subcommand: String,
    repo_id: Option<String>,
    domain: Option<String>,
    results_root: PathBuf,
    parameters: Value,
    control: RunvaultBlock,
    data: Vec<Dataset>,
    origin: Origin,
    visibility: Visibility,
    repo_root: Option<PathBuf>,
    started_from: Option<PathBuf>,
    master_seed: Option<u64>,
    replicate_index: Option<u64>,
    llm: Option<Llm>,
    lineage: Option<Lineage>,
    research: Research,
    ext: Option<Map<String, Value>>,
    cli_args: Option<Vec<String>>,
    python_version: Option<String>,
}

impl RunOptions {
    /// Starts describing a run of `subcommand` within `experiment`.
    pub fn new(experiment: impl Into<String>, subcommand: impl Into<String>) -> Self {
        Self {
            experiment: experiment.into(),
            subcommand: subcommand.into(),
            repo_id: None,
            domain: None,
            results_root: PathBuf::from("results"),
            parameters: Value::Object(Map::new()),
            control: RunvaultBlock::default(),
            data: Vec::new(),
            origin: Origin::Code,
            visibility: Visibility::Internal,
            repo_root: None,
            started_from: None,
            master_seed: None,
            replicate_index: None,
            llm: None,
            lineage: None,
            research: Research::default(),
            ext: None,
            cli_args: None,
            python_version: None,
        }
    }

    /// The stable id of the repository. Not derived from the git remote, which gets renamed.
    pub fn repo_id(mut self, repo_id: impl Into<String>) -> Self {
        self.repo_id = Some(repo_id.into());
        self
    }

    /// The field of study (`simulation` / `llm-safety` / `anomaly-detection` / ...).
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Where `<experiment>/<run_slug>/` goes. Defaults to `results`.
    pub fn results_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.results_root = root.into();
        self
    }

    /// The experimental condition. Must serialize to a JSON object.
    pub fn parameters<T: Serialize + ?Sized>(mut self, parameters: &T) -> Result<Self> {
        let value = serde_json::to_value(parameters)?;
        if !value.is_object() {
            return Err(Error::spec(
                "parameters は JSON オブジェクトである必要があります",
            ));
        }
        self.parameters = value;
        Ok(self)
    }

    /// Pointers removed from every hash (`/output_dir`, `/log_level`, ...).
    pub fn hash_exclude<I, S>(mut self, pointers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.control
            .hash_exclude
            .extend(pointers.into_iter().map(Into::into));
        self
    }

    /// Where the seeds live, so a replicate shares its condition but not its execution.
    pub fn seed_pointers<I, S>(mut self, pointers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.control
            .seed_pointers
            .extend(pointers.into_iter().map(Into::into));
        self
    }

    /// Pointers the experiment declares do not change the result.
    ///
    /// Declared, never guessed: excluding `/threads` unconditionally would bundle
    /// runs whose results genuinely differ as one condition.
    pub fn invariant_to<I, S>(mut self, pointers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.control
            .determinism
            .invariant_to
            .extend(pointers.into_iter().map(Into::into));
        self
    }

    /// Globs added to what `runvault sync` sends.
    pub fn sync_include<I, S>(mut self, globs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.control
            .sync_include
            .extend(globs.into_iter().map(Into::into));
        self
    }

    /// Globs kept out of what `runvault sync` sends. Wins over `sync_include`.
    pub fn sync_exclude<I, S>(mut self, globs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.control
            .sync_exclude
            .extend(globs.into_iter().map(Into::into));
        self
    }

    /// The datasets the run used. An empty list means "none", not "not recorded".
    pub fn data(mut self, data: impl IntoIterator<Item = Dataset>) -> Self {
        self.data.extend(data);
        self
    }

    /// Where the record came from. Defaults to [`Origin::Code`].
    pub fn origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    /// Whether the run may be synced. Defaults to [`Visibility::Internal`].
    pub fn visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// The repository whose commit and working tree get recorded.
    /// Defaults to the repository the current directory is in.
    pub fn repo_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.repo_root = Some(root.into());
        self
    }

    /// The seed every other stream derives from. Required for `domain = simulation`.
    pub fn master_seed(mut self, seed: u64) -> Self {
        self.master_seed = Some(seed);
        self
    }

    /// Which repeat of the same condition this run is.
    pub fn replicate_index(mut self, index: u64) -> Self {
        self.replicate_index = Some(index);
        self
    }

    /// The model under test. Required for `domain = llm-safety`.
    pub fn llm(mut self, llm: Llm) -> Self {
        self.llm = Some(llm);
        self
    }

    /// How this run relates to others.
    pub fn lineage(mut self, lineage: Lineage) -> Self {
        self.lineage = Some(lineage);
        self
    }

    /// The paper and targets this run reproduces.
    pub fn replication(mut self, replication: impl Into<Replication>) -> Self {
        self.research = replication.into().into();
        self
    }

    /// Field-specific metadata, so the top level never grows a block per field.
    pub fn ext(mut self, ext: Map<String, Value>) -> Self {
        self.ext = Some(ext);
        self
    }

    /// How the program was invoked. Defaults to the process's own arguments.
    pub fn cli_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.cli_args = Some(args.into_iter().map(Into::into).collect());
        self
    }

    /// The Python interpreter, for a run that used one.
    pub fn python_version(mut self, version: impl Into<String>) -> Self {
        self.python_version = Some(version.into());
        self
    }

    fn require(&self) -> Result<(&str, &str)> {
        let repo_id = self
            .repo_id
            .as_deref()
            .ok_or_else(|| Error::spec("repo_id が必要です (RunOptions::repo_id)"))?;
        let domain = self
            .domain
            .as_deref()
            .ok_or_else(|| Error::spec("domain が必要です (RunOptions::domain)"))?;
        ids::validate_slug("repo_id", repo_id)?;
        ids::validate_slug("experiment", &self.experiment)?;
        ids::validate_slug("subcommand", &self.subcommand)?;
        ids::validate_slug("domain", domain)?;

        if domain == "simulation" && self.master_seed.is_none() {
            return Err(Error::spec(
                "domain=simulation では master_seed が必要です (RunOptions::master_seed)",
            ));
        }
        if domain == "llm-safety" && self.llm.is_none() {
            return Err(Error::spec(
                "domain=llm-safety では llm が必要です (RunOptions::llm)",
            ));
        }

        let mut seen = BTreeSet::new();
        for d in &self.data {
            ids::validate_slug("data[].role", &d.role)?;
            ids::validate_slug("data[].name", &d.name)?;
            if !seen.insert((&d.role, &d.name)) {
                return Err(Error::spec(format!(
                    "data[] の (role, name) が重複しています: ({}, {})",
                    d.role, d.name
                )));
            }
            if d.hash.is_none() && d.dataset_id.is_none() && d.uri.is_none() {
                return Err(Error::spec(format!(
                    "data[] の ({}, {}) は hash / dataset_id / uri のいずれかが必要です",
                    d.role, d.name
                )));
            }
        }

        verify::check_research(&self.research)?;
        verify::check_lineage_shape(self.lineage.as_ref())?;
        Ok((repo_id, domain))
    }
}

/// A run in progress. Every write appends; nothing already written is revised.
pub struct Run {
    dir: PathBuf,
    experiment_dir: PathBuf,
    meta: RunMeta,
    started_at: DateTime<Local>,
    collision_index: Option<u64>,
    metrics: Option<csv::Writer<File>>,
    reference: Option<csv::Writer<File>>,
    events: Option<BufWriter<File>>,
    counts: Counts,
    heartbeat: Option<Heartbeat>,
    finished: bool,
}

impl Run {
    /// Creates the run directory and writes `run.json` and `config.json`.
    pub fn start(options: RunOptions) -> Result<Self> {
        let (repo_id, domain) = options.require()?;
        let repo_id = repo_id.to_string();
        let domain = domain.to_string();
        let now = Local::now();

        let code_needed = options.origin == Origin::Code;
        let repo_root = if code_needed {
            let from = match &options.repo_root {
                Some(root) => root.clone(),
                None => std::env::current_dir().map_err(Error::PlainIo)?,
            };
            Some(crate::git::repo_root(&from).map_err(|e| {
                Error::spec(format!(
                    "origin=code ですが git リポジトリが見つかりません ({}): {e}",
                    from.display()
                ))
            })?)
        } else {
            None
        };

        let planned_locks = match &repo_root {
            Some(root) => crate::git::plan_locks(root)?,
            None => Vec::new(),
        };
        let locks: Vec<_> = planned_locks.iter().map(|(_, lock)| lock.clone()).collect();

        let env: Env = crate::env::collect(options.python_version.clone(), &locks);
        let code: Option<Code> = match &repo_root {
            Some(root) => {
                let started_from = options
                    .started_from
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| root.clone());
                Some(crate::git::collect(root, &started_from, locks)?)
            }
            None => None,
        };

        let exclusions = Exclusions::resolve(&options.control, &options.parameters)?;
        let config_hash = config_hash(&options.parameters, &exclusions, &options.data)?;
        let execution_hash = execution_hash(
            &config_hash,
            &options.parameters,
            &exclusions,
            code.as_ref(),
            &env.env_hash,
        )?;

        let run_uid = ids::new_run_uid(now);
        let timestamp = ids::timestamp_part(now);
        let experiment_dir =
            crate::paths::experiment_dir(&options.results_root, &options.experiment);
        let (dir, run_slug, collision_index) = create_run_dir(
            &experiment_dir,
            &options.subcommand,
            &timestamp,
            &config_hash,
            &execution_hash,
        )?;

        crate::git::materialize_locks(&planned_locks, &dir)?;

        let meta = RunMeta {
            schema_version: SCHEMA_VERSION.into(),
            vocab_version: vocabulary::get().version.clone(),
            runvault_version: crate::env::runvault_version().into(),
            run_uid: run_uid.clone(),
            run_slug: run_slug.clone(),
            repo_id,
            experiment: options.experiment.clone(),
            subcommand: options.subcommand.clone(),
            domain: domain.clone(),
            config_hash,
            execution_hash,
            created_at: now.to_rfc3339(),
            cli_args: options
                .cli_args
                .clone()
                .unwrap_or_else(|| std::env::args().collect()),
            origin: options.origin,
            visibility: options.visibility,
            code,
            env,
            rng: rng_of(&options, &domain),
            llm: options.llm.clone(),
            data: options.data.clone(),
            lineage: options.lineage.clone(),
            research: options.research.clone(),
            ext: options.ext.clone(),
        };

        let envelope = ConfigEnvelope {
            schema_version: SCHEMA_VERSION.into(),
            run_uid: run_uid.clone(),
            runvault: options.control.clone(),
            parameters: options.parameters.clone(),
        };

        // From here the directory exists, so a failure must not leave something
        // that has neither a status nor a lock: `gc` would see no lock and walk
        // past it, and the half-written run would sit there for good.
        let started = (|| -> Result<Heartbeat> {
            files::write_json_atomically(&dir.join("config.json"), &envelope)?;
            files::write_json_atomically(&dir.join("run.json"), &meta)?;
            Heartbeat::start(&dir, LockRecord::for_this_process(now))
        })();

        let heartbeat = match started {
            Ok(heartbeat) => heartbeat,
            Err(e) => {
                record_failed_start(&dir, &run_uid, now, collision_index, &e);
                return Err(e);
            }
        };

        Ok(Self {
            dir,
            experiment_dir,
            meta,
            started_at: now,
            collision_index,
            metrics: None,
            reference: None,
            events: None,
            counts: Counts::default(),
            heartbeat: Some(heartbeat),
            finished: false,
        })
    }

    /// The run directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The run's primary key.
    pub fn run_uid(&self) -> &str {
        &self.meta.run_uid
    }

    /// The directory name.
    pub fn run_slug(&self) -> &str {
        &self.meta.run_slug
    }

    /// Everything `run.json` holds.
    pub fn meta(&self) -> &RunMeta {
        &self.meta
    }

    /// Records a number. Call `.send()` on the returned builder.
    pub fn log_metric(&mut self, name: impl Into<String>, value: f64) -> MetricEntry<'_> {
        MetricEntry {
            run: self,
            name: name.into(),
            value,
            step: None,
            step_unit: None,
            scope: DEFAULT_SCOPE.into(),
        }
    }

    /// Records the value the paper reports, so the difference can be computed later.
    pub fn log_reference(&mut self, name: impl Into<String>, value: f64) -> ReferenceEntry<'_> {
        ReferenceEntry {
            run: self,
            name: name.into(),
            value,
            step: None,
            step_unit: None,
            scope: DEFAULT_SCOPE.into(),
            target_id: None,
            source: None,
        }
    }

    /// Appends one line to `events.jsonl`.
    ///
    /// A record that calls itself `observation` or `terminal` must carry the
    /// reserved keys those kinds mean, so a terminal line cannot be terminal in
    /// name only.
    pub fn log_event<T: Serialize + ?Sized>(&mut self, kind: &str, payload: &T) -> Result<()> {
        let value = serde_json::to_value(payload)?;
        let Value::Object(mut object) = value else {
            return Err(Error::spec(
                "イベントの payload は JSON オブジェクトである必要があります",
            ));
        };
        check_event_kind(kind)?;

        object.insert("schema".into(), Value::String(kind.to_string()));
        object.insert("run_uid".into(), Value::String(self.meta.run_uid.clone()));
        object
            .entry("ts")
            .or_insert_with(|| Value::String(Local::now().to_rfc3339()));
        check_event_reserved(kind, &object)?;

        let writer = match &mut self.events {
            Some(w) => w,
            None => {
                let file = append_file(&self.dir.join("events.jsonl"))?;
                self.events.insert(BufWriter::new(file))
            }
        };
        let line = serde_json::to_string(&Value::Object(object))?;
        writeln!(writer, "{line}").map_err(Error::PlainIo)?;
        writer.flush().map_err(Error::PlainIo)?;
        self.counts.events += 1;
        Ok(())
    }

    /// Writes `manifest.csv`, checks the run against itself and writes `status.json`.
    ///
    /// Only the shallow checks run here: rehashing the data and walking
    /// `events.jsonl` costs time proportional to the run, and belongs before a
    /// sync or before a table is built, not at the end of every execution.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.finish_inner()?;
        Ok(self.dir.clone())
    }

    /// Ends the run as failed, with a reason.
    ///
    /// Tears down in the same order as [`Run::finish`]: the heartbeat stops and
    /// the lock goes before `status.json` is written, so an explicit failure
    /// never leaves the two together.
    pub fn fail(mut self, kind: impl Into<String>, message: impl Into<String>) -> Result<PathBuf> {
        let error = StatusError {
            kind: kind.into(),
            message: message.into(),
        };
        self.release_lock()?;
        self.write_status(State::Failed, Some(error), None)?;
        self.finished = true;
        Ok(self.dir.clone())
    }

    /// Stops the heartbeat and removes the lock. Always before `status.json`.
    fn release_lock(&mut self) -> Result<()> {
        if let Some(mut beat) = self.heartbeat.take() {
            beat.stop();
        }
        crate::lockfile::remove(&self.dir)
    }

    fn finish_inner(&mut self) -> Result<()> {
        self.flush_writers()?;
        self.write_manifest()?;

        // The lock goes before `status.json`: a completed run must never be found
        // with both, which is one of the invariants `verify` checks.
        self.release_lock()?;

        match verify::shallow(&self.dir) {
            Ok(()) => {
                self.write_status(State::Finished, None, Some(0))?;
                self.finished = true;
                let finished_at = self.status_finished_at();
                crate::paths::update_latest_finished(
                    &self.experiment_dir,
                    &self.meta.run_slug,
                    &finished_at,
                )?;
                Ok(())
            }
            Err(e) => {
                let error = StatusError {
                    kind: "verify".into(),
                    message: e.to_string(),
                };
                self.write_status(State::Failed, Some(error), None)?;
                self.finished = true;
                Err(e)
            }
        }
    }

    fn status_finished_at(&self) -> String {
        files::read_json::<RunStatus>(&self.dir.join("status.json"))
            .map(|s| s.finished_at)
            .unwrap_or_else(|_| Local::now().to_rfc3339())
    }

    fn flush_writers(&mut self) -> Result<()> {
        if let Some(w) = &mut self.metrics {
            w.flush().map_err(Error::PlainIo)?;
        }
        if let Some(w) = &mut self.reference {
            w.flush().map_err(Error::PlainIo)?;
        }
        if let Some(w) = &mut self.events {
            w.flush().map_err(Error::PlainIo)?;
        }
        Ok(())
    }

    fn write_manifest(&mut self) -> Result<()> {
        let mut rows = Vec::new();
        for sub in ["artifacts", "logs"] {
            for rel in files::walk_files(&self.dir.join(sub), &self.dir)? {
                let (digest, bytes) = files::digest_file(&self.dir.join(&rel))?;
                rows.push((rel, digest, bytes));
            }
        }
        rows.sort();

        let mut writer =
            csv::Writer::from_path(self.dir.join("manifest.csv")).map_err(Error::Csv)?;
        writer.write_record(MANIFEST_HEADER).map_err(Error::Csv)?;
        for (path, digest, bytes) in &rows {
            writer
                .write_record([
                    &self.meta.run_uid,
                    path,
                    &"blake3".to_string(),
                    digest,
                    &bytes.to_string(),
                ])
                .map_err(Error::Csv)?;
        }
        writer.flush().map_err(Error::PlainIo)?;
        self.counts.artifacts = rows.len() as u64;
        Ok(())
    }

    fn write_status(
        &mut self,
        state: State,
        error: Option<StatusError>,
        exit_code: Option<i64>,
    ) -> Result<()> {
        let now = Local::now();
        let status = RunStatus {
            schema_version: SCHEMA_VERSION.into(),
            run_uid: self.meta.run_uid.clone(),
            state,
            started_at: self.started_at.to_rfc3339(),
            finished_at: now.to_rfc3339(),
            duration_sec: (now - self.started_at).num_milliseconds() as f64 / 1000.0,
            exit_code,
            collision_index: self.collision_index,
            error,
            counts: Some(self.counts.clone()),
        };
        files::write_json_atomically(&self.dir.join("status.json"), &status)
    }

    fn append_metric_row(&mut self, row: [String; 6]) -> Result<()> {
        let writer = match &mut self.metrics {
            Some(w) => w,
            None => {
                let file = append_file(&self.dir.join("metrics.csv"))?;
                let mut w = csv::Writer::from_writer(file);
                w.write_record(METRICS_HEADER).map_err(Error::Csv)?;
                self.metrics.insert(w)
            }
        };
        writer.write_record(&row).map_err(Error::Csv)?;
        writer.flush().map_err(Error::PlainIo)?;
        self.counts.metrics += 1;
        Ok(())
    }

    fn append_reference_row(&mut self, row: [String; 8]) -> Result<()> {
        let writer = match &mut self.reference {
            Some(w) => w,
            None => {
                let file = append_file(&self.dir.join("reference.csv"))?;
                let mut w = csv::Writer::from_writer(file);
                w.write_record(REFERENCE_HEADER).map_err(Error::Csv)?;
                self.reference.insert(w)
            }
        };
        writer.write_record(&row).map_err(Error::Csv)?;
        writer.flush().map_err(Error::PlainIo)?;
        Ok(())
    }
}

impl Drop for Run {
    /// A run that was not finished explicitly is a run that failed.
    ///
    /// This does not cover SIGKILL or a power cut, which is why the lock file
    /// carries a heartbeat and `runvault gc` exists.
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.release_lock();
        let _ = self.write_status(
            State::Failed,
            Some(StatusError {
                kind: "dropped".into(),
                message: "finish() が呼ばれないまま Run が drop されました".into(),
            }),
            None,
        );
    }
}

/// A metric waiting for its axis and scope.
#[must_use = "the row is only written by send()"]
pub struct MetricEntry<'a> {
    run: &'a mut Run,
    name: String,
    value: f64,
    step: Option<u64>,
    step_unit: Option<String>,
    scope: String,
}

impl MetricEntry<'_> {
    /// Places the value on a time axis. Aggregated values have neither.
    pub fn step(mut self, step: u64, unit: impl Into<String>) -> Self {
        self.step = Some(step);
        self.step_unit = Some(unit.into());
        self
    }

    /// How coarse the value is (`run` / `sweep` / `trial` / `agent` / `flow`).
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// Appends the row.
    pub fn send(self) -> Result<()> {
        check_metric(
            &self.name,
            &self.scope,
            self.step_unit.as_deref(),
            self.value,
        )?;
        let row = [
            self.run.meta.run_uid.clone(),
            self.step.map(|s| s.to_string()).unwrap_or_default(),
            self.step_unit.clone().unwrap_or_default(),
            self.scope.clone(),
            self.name.clone(),
            crate::canonical::format_f64(self.value)?,
        ];
        self.run.append_metric_row(row)
    }
}

/// A value the paper reports, waiting for the target it belongs to.
#[must_use = "the row is only written by send()"]
pub struct ReferenceEntry<'a> {
    run: &'a mut Run,
    name: String,
    value: f64,
    step: Option<u64>,
    step_unit: Option<String>,
    scope: String,
    target_id: Option<String>,
    source: Option<String>,
}

impl ReferenceEntry<'_> {
    /// Places the value on a time axis.
    pub fn step(mut self, step: u64, unit: impl Into<String>) -> Self {
        self.step = Some(step);
        self.step_unit = Some(unit.into());
        self
    }

    /// How coarse the value is.
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// Which target experiment the value comes from.
    pub fn target(mut self, target_id: impl Into<String>) -> Self {
        self.target_id = Some(target_id.into());
        self
    }

    /// Where in the paper it was read (`Table 3 row 2`).
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Appends the row.
    ///
    /// A value read off a figure has no place here: recording an estimate as a
    /// reported value makes the two indistinguishable afterwards.
    pub fn send(self) -> Result<()> {
        check_metric(
            &self.name,
            &self.scope,
            self.step_unit.as_deref(),
            self.value,
        )?;
        let target_id = self
            .target_id
            .clone()
            .ok_or_else(|| Error::spec("reference には target を指定してください"))?;
        let source = self
            .source
            .clone()
            .ok_or_else(|| Error::spec("reference には source (値の出典) を指定してください"))?;
        crate::ids::validate_slug("reference target_id", &target_id)?;
        if !self
            .run
            .meta
            .research
            .targets
            .iter()
            .any(|t| t.target_id == target_id)
        {
            return Err(Error::spec(format!(
                "target_id `{target_id}` は research.targets[] にありません"
            )));
        }
        let row = [
            self.run.meta.run_uid.clone(),
            self.step.map(|s| s.to_string()).unwrap_or_default(),
            self.step_unit.clone().unwrap_or_default(),
            self.scope.clone(),
            self.name.clone(),
            crate::canonical::format_f64(self.value)?,
            target_id,
            source,
        ];
        self.run.append_reference_row(row)
    }
}

/// Marks a run that could not finish starting, so it is not left in limbo.
///
/// Best effort: this runs on a path that is already failing, and a second
/// failure here must not replace the error the caller needs to see.
fn record_failed_start(
    dir: &Path,
    run_uid: &str,
    started_at: DateTime<Local>,
    collision_index: Option<u64>,
    cause: &Error,
) {
    let _ = crate::lockfile::remove(dir);
    let now = Local::now();
    let status = RunStatus {
        schema_version: SCHEMA_VERSION.into(),
        run_uid: run_uid.to_string(),
        state: State::Failed,
        started_at: started_at.to_rfc3339(),
        finished_at: now.to_rfc3339(),
        duration_sec: (now - started_at).num_milliseconds().max(0) as f64 / 1000.0,
        exit_code: None,
        collision_index,
        error: Some(StatusError {
            kind: "start".into(),
            message: format!("run の作成を完了できませんでした: {cause}"),
        }),
        counts: None,
    };
    let _ = files::write_json_atomically(&dir.join("status.json"), &status);
}

fn rng_of(options: &RunOptions, domain: &str) -> Option<crate::meta::Rng> {
    if options.master_seed.is_none() && options.replicate_index.is_none() && domain != "simulation"
    {
        return None;
    }
    Some(crate::meta::Rng {
        master_seed: options.master_seed,
        replicate_index: options.replicate_index,
    })
}

fn append_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::io(path, e))
}

/// Creates `<experiment>/<slug>`, adding `-N` until the name is free.
///
/// A parallel sweep of the same condition with different seeds starts many runs
/// in the same second; without the suffix they would all want one directory.
fn create_run_dir(
    experiment_dir: &Path,
    subcommand: &str,
    timestamp: &str,
    config_hash: &str,
    execution_hash: &str,
) -> Result<(PathBuf, String, Option<u64>)> {
    std::fs::create_dir_all(experiment_dir).map_err(|e| Error::io(experiment_dir, e))?;
    for collision_index in std::iter::once(None).chain((2..=MAX_COLLISION_INDEX).map(Some)) {
        let slug = ids::run_slug(
            subcommand,
            timestamp,
            config_hash,
            execution_hash,
            collision_index,
        );
        let dir = experiment_dir.join(&slug);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((dir, slug, collision_index)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io(&dir, e)),
        }
    }
    Err(Error::spec(format!(
        "{} に空いている run ディレクトリ名がありません",
        experiment_dir.display()
    )))
}

fn check_metric(name: &str, scope: &str, step_unit: Option<&str>, value: f64) -> Result<()> {
    ids::validate_slug("指標名", name)?;
    ids::validate_slug("scope", scope)?;
    if let Some(unit) = step_unit {
        ids::validate_slug("step_unit", unit)?;
    }
    if !value.is_finite() {
        return Err(Error::spec(format!(
            "指標 `{name}` に NaN / Inf は書けません (欠測は行を書かない)"
        )));
    }
    let vocab = vocabulary::get();
    if !vocab.metric_allowed_at(name, scope) {
        let allowed = vocab.metric_names[name].scopes.join(" / ");
        return Err(Error::spec(format!(
            "予約指標 `{name}` は scope={allowed} でのみ使えます (scope={scope} で書こうとしました)"
        )));
    }
    Ok(())
}

fn check_event_kind(kind: &str) -> Result<()> {
    if vocabulary::get().event_schemas.iter().any(|s| s == kind) {
        return Ok(());
    }
    if let Some(rest) = kind.strip_prefix("x.")
        && let Some((repo, name)) = rest.split_once('.')
    {
        ids::validate_slug("イベント種別の repo_id", repo)?;
        ids::validate_slug("イベント種別の名前", name)?;
        return Ok(());
    }
    Err(Error::spec(format!(
        "イベント種別 `{kind}` はコア語彙にも x.<repo_id>.<name> にも当てはまりません"
    )))
}

/// The reserved keys each core event kind means.
fn check_event_reserved(kind: &str, object: &Map<String, Value>) -> Result<()> {
    let required: &[&str] = match kind {
        "observation" => &["unit_id", "t", "t_unit"],
        "terminal" => &["unit_id", "t", "t_unit", "outcome", "censored", "budget"],
        _ => return Ok(()),
    };
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|key| object.get(*key).is_none_or(Value::is_null))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::spec(format!(
            "`{kind}` を名乗るイベントには {} が必要です",
            missing.join(" / ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colliding_directory_name_gets_the_next_suffix() {
        // A parallel sweep of the same condition with different seeds starts many
        // runs in the same second, and every one of them wants this same name.
        let root = tempfile::tempdir().unwrap();
        let exp = root.path().join("schelling");
        let cfg = "9f2c41ab".repeat(8);
        let exec = "3b1d".repeat(16);

        let (dir, slug, index) =
            create_run_dir(&exp, "main", "20260830_101500", &cfg, &exec).unwrap();
        assert_eq!(slug, "main_20260830_101500_9f2c41ab_3b1d");
        assert_eq!(index, None);
        assert!(dir.is_dir());

        let (_, slug, index) =
            create_run_dir(&exp, "main", "20260830_101500", &cfg, &exec).unwrap();
        assert_eq!(slug, "main_20260830_101500_9f2c41ab_3b1d-2");
        assert_eq!(index, Some(2));

        let (_, slug, index) =
            create_run_dir(&exp, "main", "20260830_101500", &cfg, &exec).unwrap();
        assert_eq!(slug, "main_20260830_101500_9f2c41ab_3b1d-3");
        assert_eq!(index, Some(3));
    }

    #[test]
    fn a_start_that_fails_after_the_directory_exists_leaves_a_record() {
        // Without this the directory has neither a status nor a lock, and `gc`
        // walks straight past it: the half-written run stays for good.
        let dir = tempfile::tempdir().unwrap();
        record_failed_start(
            dir.path(),
            "01K3QZ8F7H9M2N4P6R8T0V2X4Z",
            Local::now(),
            Some(2),
            &Error::spec("disk full"),
        );
        let status: RunStatus =
            files::read_json(&dir.path().join("status.json")).expect("status was written");
        assert_eq!(status.state, State::Failed);
        assert_eq!(status.collision_index, Some(2));
        let error = status.error.expect("a failed run states why");
        assert_eq!(error.kind, "start");
        assert!(error.message.contains("disk full"), "{}", error.message);
        assert!(!dir.path().join(crate::lockfile::LOCK_FILE).exists());
    }

    #[test]
    fn a_different_execution_needs_no_suffix() {
        let root = tempfile::tempdir().unwrap();
        let exp = root.path().join("schelling");
        let cfg = "9f2c41ab".repeat(8);
        create_run_dir(&exp, "main", "20260830_101500", &cfg, &"3b1d".repeat(16)).unwrap();
        let (_, slug, index) =
            create_run_dir(&exp, "main", "20260830_101500", &cfg, &"c0de".repeat(16)).unwrap();
        assert_eq!(slug, "main_20260830_101500_9f2c41ab_c0de");
        assert_eq!(index, None);
    }
}
