//! Small filesystem helpers shared by the writer, `verify` and `gc`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::canonical::blake3_hex;
use crate::error::{Error, Result};
use crate::meta::Algorithm;

/// Writes bytes by creating a temporary file next to `path` and renaming it.
///
/// A reader must never see a half-written `status.json` and conclude the run
/// finished, so the file appears complete or not at all.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;

    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
    Ok(())
}

/// Serializes to pretty JSON and writes it atomically, with a trailing newline.
pub fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_atomically(path, &bytes)
}

/// Reads and parses a JSON file.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_str(&text)
        .map_err(|e| Error::spec(format!("{}: JSON として読めません: {e}", path.display())))
}

/// BLAKE3 and byte count of a file. What `runvault` records for its own files.
pub fn digest_file(path: &Path) -> Result<(String, u64)> {
    digest_file_as(path, Algorithm::Blake3)
}

/// The digest of a file under the function a record says it used, and its size.
///
/// `runvault` writes BLAKE3, but `schema/v1` also accepts SHA-256 so a record
/// can carry a digest that came from somewhere else. Both are computed here:
/// checking only the one this crate prefers would leave the other rows
/// unexamined while `verify` still reported the run as verified.
pub fn digest_file_as(path: &Path, algorithm: Algorithm) -> Result<(String, u64)> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    let digest = match algorithm {
        Algorithm::Blake3 => blake3_hex(&bytes),
        Algorithm::Sha256 => {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(&bytes);
            hasher
                .finalize()
                .iter()
                .fold(String::with_capacity(64), |mut out, byte| {
                    use std::fmt::Write;
                    let _ = write!(out, "{byte:02x}");
                    out
                })
        }
    };
    Ok((digest, bytes.len() as u64))
}

/// Every regular file under `root`, as paths relative to `base`, sorted.
///
/// Sorting makes `manifest.csv` reproducible; symbolic links are not followed,
/// so a link out of the run directory cannot pull foreign files into the record.
pub fn walk_files(root: &Path, base: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                out.push(relative(&path, base)?);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `path` relative to `base`, with `/` separators.
pub fn relative(path: &Path, base: &Path) -> Result<String> {
    let rel: PathBuf = path
        .strip_prefix(base)
        .map_err(|_| {
            Error::spec(format!(
                "{} は {} の下にありません",
                path.display(),
                base.display()
            ))
        })?
        .to_path_buf();
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_atomic_write_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        write_atomically(&path, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, ["status.json"]);
    }

    #[test]
    fn an_atomic_write_replaces_the_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        write_atomically(&path, b"one").unwrap();
        write_atomically(&path, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn walking_lists_nested_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts/figs")).unwrap();
        std::fs::write(dir.path().join("artifacts/b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("artifacts/figs/a.svg"), "a").unwrap();
        let files = walk_files(&dir.path().join("artifacts"), dir.path()).unwrap();
        assert_eq!(files, ["artifacts/b.txt", "artifacts/figs/a.svg"]);
    }

    #[test]
    fn walking_a_missing_directory_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            walk_files(&dir.path().join("logs"), dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let inside = dir.path().join("artifacts");
        std::fs::create_dir_all(&inside).unwrap();
        std::os::unix::fs::symlink(&outside, inside.join("link.txt")).unwrap();
        assert!(walk_files(&inside, dir.path()).unwrap().is_empty());
    }
}
