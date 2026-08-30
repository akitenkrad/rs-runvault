//! What the working tree looked like when the run started.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::canonical::{blake3_hex, push_lp};
use crate::error::{Error, Result};
use crate::meta::{Code, Hash, Lock, LockKind};

/// The lock files that get copied into the run's `lock/` directory.
const LOCK_FILES: [(&str, LockKind); 4] = [
    ("Cargo.lock", LockKind::Cargo),
    ("uv.lock", LockKind::Uv),
    ("poetry.lock", LockKind::Poetry),
    ("requirements.lock", LockKind::Pip),
];

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::io(dir, e))?;
    if !out.status.success() {
        return Err(Error::spec(format!(
            "git {} が失敗しました ({}): {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string())
}

fn git_opt(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).ok().filter(|s| !s.is_empty())
}

/// The root of the repository `dir` lives in.
pub fn repo_root(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?))
}

/// Hashes the working-tree difference from `HEAD`.
///
/// `git diff HEAD` does not list untracked files, so experiment code that has
/// not been `git add`ed yet would otherwise change the result without changing
/// the hash. Their paths and contents are folded in, in path order.
pub fn dirty_hash(root: &Path) -> Result<Hash> {
    let diff = git(root, &["diff", "HEAD"])?;
    let mut untracked: Vec<String> = git(root, &["ls-files", "--others", "--exclude-standard"])?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    untracked.sort();

    let mut blob = Vec::new();
    push_lp(&mut blob, diff.as_bytes());
    for rel in &untracked {
        let path = root.join(rel);
        let content = std::fs::read(&path).unwrap_or_default();
        push_lp(&mut blob, rel.as_bytes());
        push_lp(&mut blob, &content);
    }
    Ok(Hash::blake3(blake3_hex(&blob)))
}

/// Whether the working tree differs from `HEAD`, counting untracked files.
pub fn is_dirty(root: &Path) -> Result<bool> {
    let diff = git(root, &["diff", "HEAD"])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard"])?;
    Ok(!diff.is_empty() || !untracked.is_empty())
}

/// Describes the repository's lock files without copying them yet.
///
/// The copies land inside the run directory, whose name depends on
/// `execution_hash`, which depends on these hashes: the description has to come
/// first, and the copying after the directory exists.
pub fn plan_locks(root: &Path) -> Result<Vec<(PathBuf, Lock)>> {
    let mut planned = Vec::new();
    for (name, kind) in LOCK_FILES {
        let src = root.join(name);
        if !src.is_file() {
            continue;
        }
        let bytes = std::fs::read(&src).map_err(|e| Error::io(&src, e))?;
        planned.push((
            src,
            Lock {
                kind,
                hash: Hash::blake3(blake3_hex(&bytes)),
                file: format!("lock/{name}"),
            },
        ));
    }
    Ok(planned)
}

/// Copies the planned lock files into the run directory.
pub fn materialize_locks(planned: &[(PathBuf, Lock)], run_dir: &Path) -> Result<()> {
    for (src, lock) in planned {
        let dst = run_dir.join(&lock.file);
        let dir = dst.parent().unwrap_or(run_dir);
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        std::fs::copy(src, &dst).map_err(|e| Error::io(&dst, e))?;
    }
    Ok(())
}

/// Describes the code that is about to produce a run.
///
/// `locks` comes from [`collect_locks`], which needs the run directory and so
/// runs separately.
pub fn collect(repo_root: &Path, started_from: &Path, locks: Vec<Lock>) -> Result<Code> {
    let git_commit = git(repo_root, &["rev-parse", "HEAD"])?;
    let git_dirty = is_dirty(repo_root)?;
    let dirty_hash = if git_dirty {
        Some(dirty_hash(repo_root)?)
    } else {
        None
    };
    let repo_relpath = started_from
        .strip_prefix(repo_root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|p| !p.is_empty());

    Ok(Code {
        git_remote: git_opt(repo_root, &["config", "--get", "remote.origin.url"]),
        git_branch: git_opt(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        git_commit,
        git_dirty,
        dirty_hash,
        repo_relpath,
        locks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q"]).unwrap();
        git(p, &["config", "user.email", "t@example.com"]).unwrap();
        git(p, &["config", "user.name", "t"]).unwrap();
        std::fs::write(p.join("a.txt"), "one").unwrap();
        git(p, &["add", "-A"]).unwrap();
        git(p, &["commit", "-qm", "first"]).unwrap();
        dir
    }

    #[test]
    fn a_clean_tree_has_no_dirty_hash() {
        let dir = repo();
        let code = collect(dir.path(), dir.path(), vec![]).unwrap();
        assert!(!code.git_dirty);
        assert!(code.dirty_hash.is_none());
        assert_eq!(code.git_commit.len(), 40);
    }

    #[test]
    fn an_untracked_file_makes_the_tree_dirty() {
        let dir = repo();
        std::fs::write(dir.path().join("scratch.rs"), "fn main() {}").unwrap();
        let code = collect(dir.path(), dir.path(), vec![]).unwrap();
        assert!(code.git_dirty, "git diff HEAD alone would have missed this");
        assert!(code.dirty_hash.is_some());
    }

    #[test]
    fn editing_an_untracked_file_changes_the_dirty_hash() {
        let dir = repo();
        std::fs::write(dir.path().join("scratch.rs"), "fn main() {}").unwrap();
        let before = dirty_hash(dir.path()).unwrap();
        std::fs::write(dir.path().join("scratch.rs"), "fn main() { work() }").unwrap();
        assert_ne!(before, dirty_hash(dir.path()).unwrap());
    }

    #[test]
    fn editing_a_tracked_file_changes_the_dirty_hash() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "two").unwrap();
        let before = dirty_hash(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "three").unwrap();
        assert_ne!(before, dirty_hash(dir.path()).unwrap());
    }

    #[test]
    fn lock_files_are_hashed_before_they_are_copied() {
        let dir = repo();
        std::fs::write(dir.path().join("Cargo.lock"), "# lock").unwrap();
        let planned = plan_locks(dir.path()).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].1.file, "lock/Cargo.lock");

        let run = tempfile::tempdir().unwrap();
        materialize_locks(&planned, run.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(run.path().join("lock/Cargo.lock")).unwrap(),
            "# lock"
        );
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect(dir.path(), dir.path(), vec![]).is_err());
    }
}
