//! `status.json` — written last, with an atomic rename.
//!
//! A run directory without this file is a run that did not complete.

use serde::{Deserialize, Serialize};

/// How the run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// `finish()` was called and the shallow checks passed.
    Finished,
    /// The process died, or `finish()` found the run inconsistent.
    Failed,
}

/// Why the run failed. Required when `state` is `failed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StatusError {
    /// A short, stable category (`dropped` / `verify` / an experiment's own kind).
    pub kind: String,
    /// The message a human reads.
    pub message: String,
}

/// How much was recorded. A cheap sanity check for a reader.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Counts {
    /// Rows in `metrics.csv`.
    pub metrics: u64,
    /// Lines in `events.jsonl`.
    pub events: u64,
    /// Rows in `manifest.csv`.
    pub artifacts: u64,
}

/// `status.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RunStatus {
    /// Version of `schema/v1`.
    pub schema_version: String,
    /// The run this status belongs to.
    pub run_uid: String,
    /// Finished or failed.
    pub state: State,
    /// When the run started.
    pub started_at: String,
    /// When it ended.
    pub finished_at: String,
    /// Wall-clock duration. The single source of truth; `metrics.csv` does not repeat it.
    pub duration_sec: f64,
    /// Process exit code. `0` or absent for a finished run.
    pub exit_code: Option<i64>,
    /// Which `-N` suffix the directory name needed, when `run_slug` collided.
    pub collision_index: Option<u64>,
    /// Why it failed.
    pub error: Option<StatusError>,
    /// How much was recorded.
    pub counts: Option<Counts>,
}
