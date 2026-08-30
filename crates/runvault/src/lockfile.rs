//! `.runvault.lock` — how a live run is told apart from one that was killed.
//!
//! `Drop` does not run on SIGKILL or a power cut, so the existence of the lock
//! cannot mean "running": a run that died would stay running forever. The lock
//! carries a heartbeat and the identity of the process, and both are checked.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::files;

/// The lock file's name inside a run directory.
pub const LOCK_FILE: &str = ".runvault.lock";

/// How often the heartbeat is refreshed.
///
/// Short enough against the five-minute threshold that missing one refresh does
/// not make a live run look dead.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// How long a heartbeat stays believable.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// What `.runvault.lock` holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockRecord {
    /// The machine the process runs on. A PID only means something there.
    pub host: String,
    /// The process id.
    pub pid: u32,
    /// When that process started, so a recycled PID is not mistaken for it.
    pub process_start_time: Option<u64>,
    /// When the run started.
    pub created_at: String,
    /// Last refresh.
    pub heartbeat_at: String,
}

/// Whether a run without `status.json` is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The process is still there, or the heartbeat is recent enough.
    Running,
    /// Neither holds: the process died without writing `status.json`.
    Stale,
}

/// The start time of a live process, in seconds since the epoch.
pub fn process_start_time(pid: u32) -> Option<u64> {
    let mut system = sysinfo::System::new();
    let target = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    system.process(target).map(|p| p.start_time())
}

impl LockRecord {
    /// A record for the current process.
    pub fn for_this_process(created_at: DateTime<Local>) -> Self {
        let pid = std::process::id();
        Self {
            host: crate::env::host(),
            pid,
            process_start_time: process_start_time(pid),
            created_at: created_at.to_rfc3339(),
            heartbeat_at: created_at.to_rfc3339(),
        }
    }

    /// Decides whether the run behind this lock is still alive.
    ///
    /// The PID is only consulted on the machine that wrote the lock; elsewhere
    /// the heartbeat is all there is.
    pub fn liveness(&self, now: DateTime<Local>, this_host: &str) -> Liveness {
        if self.host == this_host
            && let Some(started) = process_start_time(self.pid)
            && Some(started) == self.process_start_time
        {
            return Liveness::Running;
        }
        let fresh = DateTime::parse_from_rfc3339(&self.heartbeat_at)
            .map(|beat| now.signed_duration_since(beat).num_seconds())
            .map(|age| age <= STALE_AFTER.as_secs() as i64)
            .unwrap_or(false);
        if fresh {
            Liveness::Running
        } else {
            Liveness::Stale
        }
    }
}

/// Reads a run's lock file, or `None` when there is none.
pub fn read(run_dir: &Path) -> Result<Option<LockRecord>> {
    let path = run_dir.join(LOCK_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Removes a run's lock file. Missing is success.
pub fn remove(run_dir: &Path) -> Result<()> {
    let path = run_dir.join(LOCK_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Writes the lock file, replacing it atomically so a reader never sees half of it.
pub fn write(run_dir: &Path, record: &LockRecord) -> Result<()> {
    files::write_json_atomically(&run_dir.join(LOCK_FILE), record)
}

/// The thread that keeps `heartbeat_at` fresh while the run is going.
pub struct Heartbeat {
    stop: Arc<(Mutex<bool>, Condvar)>,
    failed: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Heartbeat {
    /// Writes the lock and starts refreshing it every [`HEARTBEAT_INTERVAL`].
    pub fn start(run_dir: &Path, record: LockRecord) -> Result<Self> {
        write(run_dir, &record)?;

        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let path: PathBuf = run_dir.to_path_buf();
        let thread_stop = Arc::clone(&stop);
        let thread_failed = Arc::clone(&failed);

        let handle = std::thread::Builder::new()
            .name("runvault-heartbeat".into())
            .spawn(move || {
                let (lock, cvar) = &*thread_stop;
                let mut record = record;
                let mut guard = lock.lock().expect("heartbeat mutex");
                loop {
                    // The flag is read before waiting as well as after: a stop that
                    // arrives before the first wait would otherwise cost a full
                    // interval before the thread noticed.
                    if *guard {
                        return;
                    }
                    let (next, _) = cvar
                        .wait_timeout(guard, HEARTBEAT_INTERVAL)
                        .expect("heartbeat mutex");
                    guard = next;
                    if *guard {
                        return;
                    }
                    record.heartbeat_at = Local::now().to_rfc3339();
                    if write(&path, &record).is_err() {
                        // The run itself is still valid; only the liveness signal is lost.
                        thread_failed.store(true, Ordering::Relaxed);
                    }
                }
            })
            .map_err(Error::PlainIo)?;

        Ok(Self {
            stop,
            failed,
            handle: Some(handle),
        })
    }

    /// Whether a refresh ever failed. A stopped heartbeat only makes a live run
    /// look dead, so it is reported rather than raised.
    pub fn degraded(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        {
            let (lock, cvar) = &*self.stop;
            if let Ok(mut stop) = lock.lock() {
                *stop = true;
            }
            cvar.notify_all();
        }
        let _ = handle.join();
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_running() {
        let record = LockRecord::for_this_process(Local::now());
        assert_eq!(
            record.liveness(Local::now(), &record.host),
            Liveness::Running
        );
    }

    #[test]
    fn a_dead_process_with_an_old_heartbeat_is_stale() {
        let mut record = LockRecord::for_this_process(Local::now());
        record.pid = 0; // never a live user process
        record.process_start_time = Some(1);
        record.heartbeat_at = (Local::now() - chrono::Duration::hours(2)).to_rfc3339();
        assert_eq!(record.liveness(Local::now(), &record.host), Liveness::Stale);
    }

    #[test]
    fn a_recent_heartbeat_from_another_host_still_counts_as_running() {
        let mut record = LockRecord::for_this_process(Local::now());
        record.host = "another-machine".into();
        assert_eq!(
            record.liveness(Local::now(), "this-machine"),
            Liveness::Running
        );
    }

    #[test]
    fn a_recycled_pid_does_not_revive_a_dead_run() {
        let mut record = LockRecord::for_this_process(Local::now());
        // Same PID, but the process it names started at a different time.
        record.process_start_time = record.process_start_time.map(|t| t + 1);
        record.heartbeat_at = (Local::now() - chrono::Duration::hours(2)).to_rfc3339();
        assert_eq!(record.liveness(Local::now(), &record.host), Liveness::Stale);
    }

    #[test]
    fn the_lock_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).unwrap().is_none());
        let record = LockRecord::for_this_process(Local::now());
        write(dir.path(), &record).unwrap();
        assert_eq!(read(dir.path()).unwrap().as_ref(), Some(&record));
        remove(dir.path()).unwrap();
        assert!(read(dir.path()).unwrap().is_none());
        remove(dir.path()).unwrap(); // removing a missing lock is not an error
    }

    #[test]
    fn the_heartbeat_writes_the_lock_and_cleans_up_after_itself() {
        let dir = tempfile::tempdir().unwrap();
        let mut beat =
            Heartbeat::start(dir.path(), LockRecord::for_this_process(Local::now())).unwrap();
        assert!(dir.path().join(LOCK_FILE).is_file());
        assert!(!beat.degraded());
        beat.stop();
        beat.stop(); // idempotent
    }
}
