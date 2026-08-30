//! `runvault gc` — turning killed runs into recorded failures.
//!
//! `Drop` never runs on SIGKILL or a power cut, so a run can be left with a lock
//! and no `status.json`. Left alone it stays "running" forever and every
//! aggregate silently waits for it.

use std::path::{Path, PathBuf};

use chrono::Local;

use crate::error::Result;
use crate::files;
use crate::lockfile::{self, Liveness};
use crate::meta::{RunMeta, SCHEMA_VERSION};
use crate::status::{RunStatus, State, StatusError};

/// What `gc` did to one run directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The process is alive, or the heartbeat is recent. Left alone.
    Running,
    /// The lock was stale: it was removed and `status.json` written as failed.
    Reaped,
}

/// One run `gc` looked at.
#[derive(Debug, Clone)]
pub struct Reaped {
    /// The run directory.
    pub dir: PathBuf,
    /// What was done.
    pub outcome: Outcome,
}

/// Sweeps every run under `results_root`.
pub fn collect(results_root: &Path, dry_run: bool) -> Result<Vec<Reaped>> {
    let mut out = Vec::new();
    for dir in crate::paths::run_dirs(results_root)? {
        if let Some(reaped) = collect_one(&dir, dry_run)? {
            out.push(reaped);
        }
    }
    Ok(out)
}

/// Looks at a single run. `None` when there is nothing to decide (it completed,
/// or it never had a lock).
pub fn collect_one(run_dir: &Path, dry_run: bool) -> Result<Option<Reaped>> {
    if run_dir.join("status.json").is_file() {
        // A completed run may still carry a lock if it was killed between the two
        // writes; `verify` reports that, and removing it here is the repair.
        if lockfile::read(run_dir)?.is_some() && !dry_run {
            lockfile::remove(run_dir)?;
        }
        return Ok(None);
    }

    let Some(record) = lockfile::read(run_dir)? else {
        return Ok(None);
    };

    let host = crate::env::host();
    if record.liveness(Local::now(), &host) == Liveness::Running {
        return Ok(Some(Reaped {
            dir: run_dir.to_path_buf(),
            outcome: Outcome::Running,
        }));
    }

    if !dry_run {
        lockfile::remove(run_dir)?;
        write_failed(run_dir, &record)?;
    }
    Ok(Some(Reaped {
        dir: run_dir.to_path_buf(),
        outcome: Outcome::Reaped,
    }))
}

fn write_failed(run_dir: &Path, record: &lockfile::LockRecord) -> Result<()> {
    let uid = files::read_json::<RunMeta>(&run_dir.join("run.json"))
        .map(|m| m.run_uid)
        .unwrap_or_default();
    let status = RunStatus {
        schema_version: SCHEMA_VERSION.into(),
        run_uid: uid,
        state: State::Failed,
        started_at: record.created_at.clone(),
        finished_at: record.heartbeat_at.clone().max(record.created_at.clone()),
        duration_sec: duration_sec(&record.created_at, &record.heartbeat_at),
        exit_code: None,
        collision_index: None,
        error: Some(StatusError {
            kind: "killed".into(),
            message: format!(
                "{} の pid {} が status.json を書かずに終了しました (最終 heartbeat {})",
                record.host, record.pid, record.heartbeat_at
            ),
        }),
        counts: None,
    };
    files::write_json_atomically(&run_dir.join("status.json"), &status)?;
    // `finished_at` is the last heartbeat, not now: the run stopped being real then.
    Ok(())
}

fn duration_sec(started: &str, last_beat: &str) -> f64 {
    let parse = chrono::DateTime::parse_from_rfc3339;
    match (parse(started), parse(last_beat)) {
        (Ok(a), Ok(b)) => (b - a).num_milliseconds().max(0) as f64 / 1000.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_dir_with_lock(age_hours: i64) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut record = lockfile::LockRecord::for_this_process(Local::now());
        record.pid = 0;
        record.process_start_time = Some(1);
        record.heartbeat_at = (Local::now() - chrono::Duration::hours(age_hours)).to_rfc3339();
        record.created_at = record.heartbeat_at.clone();
        lockfile::write(dir.path(), &record).unwrap();
        dir
    }

    #[test]
    fn a_stale_lock_becomes_a_recorded_failure() {
        let dir = run_dir_with_lock(2);
        let reaped = collect_one(dir.path(), false).unwrap().unwrap();
        assert_eq!(reaped.outcome, Outcome::Reaped);
        assert!(!dir.path().join(lockfile::LOCK_FILE).exists());
        let status: RunStatus = files::read_json(&dir.path().join("status.json")).unwrap();
        assert_eq!(status.state, State::Failed);
        assert!(status.error.is_some());
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let dir = run_dir_with_lock(2);
        let reaped = collect_one(dir.path(), true).unwrap().unwrap();
        assert_eq!(reaped.outcome, Outcome::Reaped);
        assert!(dir.path().join(lockfile::LOCK_FILE).exists());
        assert!(!dir.path().join("status.json").exists());
    }

    #[test]
    fn a_live_run_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        lockfile::write(
            dir.path(),
            &lockfile::LockRecord::for_this_process(Local::now()),
        )
        .unwrap();
        let reaped = collect_one(dir.path(), false).unwrap().unwrap();
        assert_eq!(reaped.outcome, Outcome::Running);
        assert!(dir.path().join(lockfile::LOCK_FILE).exists());
    }

    #[test]
    fn a_completed_run_with_a_leftover_lock_is_repaired() {
        let dir = run_dir_with_lock(2);
        std::fs::write(dir.path().join("status.json"), "{}").unwrap();
        assert!(collect_one(dir.path(), false).unwrap().is_none());
        assert!(!dir.path().join(lockfile::LOCK_FILE).exists());
    }

    #[test]
    fn a_directory_without_a_lock_is_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect_one(dir.path(), false).unwrap().is_none());
    }
}
