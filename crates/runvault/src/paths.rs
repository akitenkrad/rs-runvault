//! Where runs live, and the `latest_finished` link.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::error::{Error, Result};
use crate::files;
use crate::status::RunStatus;

/// The link in an experiment directory that points at the last completed run.
pub const LATEST_FINISHED: &str = "latest_finished";

/// Held while `latest_finished` is compared and replaced.
const LINK_MUTEX: &str = ".latest_finished.mutex";

/// How long to wait for the mutex before assuming its holder died.
const MUTEX_STALE_AFTER: Duration = Duration::from_secs(30);

/// Makes each process's temporary link name unique within the process too.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `<results_root>/<experiment>`.
pub fn experiment_dir(results_root: &Path, experiment: &str) -> PathBuf {
    results_root.join(experiment)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

/// Points `latest_finished` at `slug`, but only if that run finished later than
/// the one it points at now.
///
/// Two runs started in either order can finish in either order; without the
/// comparison the link would walk backwards when a long run overtakes a short one.
pub fn update_latest_finished(
    experiment_dir: &Path,
    slug: &str,
    finished_at: &str,
) -> Result<bool> {
    // Reading the current link and replacing it have to be one step. Two runs
    // finishing at once would otherwise both read the same old link, and the one
    // that finished earlier could install itself last.
    let _guard = LinkMutex::acquire(experiment_dir)?;
    let link = experiment_dir.join(LATEST_FINISHED);

    if let Some(current) = read_link_status(&link)?
        && current.finished_at.as_str() >= finished_at
    {
        return Ok(false);
    }

    // A symlink cannot be replaced in place, so make a new one and rename over.
    let tmp = experiment_dir.join(format!(
        ".{LATEST_FINISHED}.tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&tmp);
    symlink(Path::new(slug), &tmp).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, &link).map_err(|e| Error::io(&link, e))?;
    Ok(true)
}

/// A directory-wide mutex, made of a file that only one creator can win.
struct LinkMutex {
    path: PathBuf,
}

impl LinkMutex {
    fn acquire(experiment_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(experiment_dir).map_err(|e| Error::io(experiment_dir, e))?;
        let path = experiment_dir.join(LINK_MUTEX);
        let deadline = std::time::Instant::now() + MUTEX_STALE_AFTER;

        loop {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A holder that died leaves the file behind; take it over once
                    // it is older than a whole update could possibly need.
                    if is_stale(&path) || std::time::Instant::now() >= deadline {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(Error::io(&path, e)),
            }
        }
    }
}

fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .and_then(|t| {
            SystemTime::now()
                .duration_since(t)
                .map_err(std::io::Error::other)
        })
        .is_ok_and(|age| age > MUTEX_STALE_AFTER)
}

impl Drop for LinkMutex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The status of whatever `latest_finished` points at, when it points anywhere.
fn read_link_status(link: &Path) -> Result<Option<RunStatus>> {
    if std::fs::symlink_metadata(link).is_err() {
        return Ok(None);
    }
    let target = std::fs::read_link(link).map_err(|e| Error::io(link, e))?;
    let dir = link.parent().unwrap_or(Path::new(".")).join(target);
    let status = dir.join("status.json");
    if !status.is_file() {
        return Ok(None);
    }
    files::read_json(&status).map(Some)
}

/// Every run directory under `results_root`, at any nesting depth.
///
/// A directory counts as a run when it holds `run.json`, `status.json` or the
/// lock: that also finds the legacy layouts, which have no `run.json`.
pub fn run_dirs(results_root: &Path) -> Result<Vec<PathBuf>> {
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
            if !entry.file_type().map_err(|e| Error::io(&path, e))?.is_dir() {
                continue;
            }
            if is_run_dir(&path) {
                out.push(path);
            } else {
                stack.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Whether a directory looks like a run rather than a grouping directory.
pub fn is_run_dir(path: &Path) -> bool {
    path.join("run.json").is_file()
        || path.join("status.json").is_file()
        || path.join(crate::lockfile::LOCK_FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::State;

    fn finished(dir: &Path, at: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let status = RunStatus {
            schema_version: "1.0".into(),
            run_uid: "01K3QZ8F7H9M2N4P6R8T0V2X4Z".into(),
            state: State::Finished,
            started_at: at.into(),
            finished_at: at.into(),
            duration_sec: 1.0,
            exit_code: Some(0),
            collision_index: None,
            error: None,
            counts: None,
        };
        files::write_json_atomically(&dir.join("status.json"), &status).unwrap();
    }

    #[test]
    fn the_link_follows_the_newest_completion() {
        let dir = tempfile::tempdir().unwrap();
        let exp = dir.path();
        finished(&exp.join("a"), "2026-08-30T10:00:00+09:00");
        assert!(update_latest_finished(exp, "a", "2026-08-30T10:00:00+09:00").unwrap());
        finished(&exp.join("b"), "2026-08-30T11:00:00+09:00");
        assert!(update_latest_finished(exp, "b", "2026-08-30T11:00:00+09:00").unwrap());
        assert_eq!(
            std::fs::read_link(exp.join(LATEST_FINISHED)).unwrap(),
            Path::new("b")
        );
    }

    #[test]
    fn a_run_that_started_first_but_finished_last_does_not_rewind_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let exp = dir.path();
        finished(&exp.join("late"), "2026-08-30T12:00:00+09:00");
        update_latest_finished(exp, "late", "2026-08-30T12:00:00+09:00").unwrap();
        finished(&exp.join("early"), "2026-08-30T09:00:00+09:00");
        assert!(!update_latest_finished(exp, "early", "2026-08-30T09:00:00+09:00").unwrap());
        assert_eq!(
            std::fs::read_link(exp.join(LATEST_FINISHED)).unwrap(),
            Path::new("late")
        );
    }

    #[test]
    fn runs_are_found_under_a_grouping_directory() {
        let dir = tempfile::tempdir().unwrap();
        finished(
            &dir.path().join("schelling/main_1"),
            "2026-08-30T10:00:00+09:00",
        );
        finished(
            &dir.path().join("schelling/main_2"),
            "2026-08-30T10:00:00+09:00",
        );
        let found = run_dirs(dir.path()).unwrap();
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_missing_results_root_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run_dirs(&dir.path().join("nope")).unwrap().is_empty());
    }
}
