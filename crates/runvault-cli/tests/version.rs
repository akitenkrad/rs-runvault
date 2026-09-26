//! The installed binary identifies the exact source revision it came from.

use std::process::Command;

#[test]
fn version_includes_the_build_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .arg("--version")
        .output()
        .expect("runvault starts");
    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("version is UTF-8");
    let version = stdout
        .strip_prefix("runvault ")
        .and_then(|value| value.strip_suffix('\n'))
        .expect("version has the command prefix and one trailing newline");
    let (package_version, commit) = version
        .split_once(" (commit ")
        .expect("version includes a commit");
    let commit = commit.strip_suffix(')').expect("commit is parenthesized");

    let version_parts: Vec<_> = package_version.split('.').collect();
    assert_eq!(version_parts.len(), 3, "unexpected version: {stdout:?}");
    assert!(
        version_parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit())),
        "unexpected version: {stdout:?}"
    );

    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    assert!(
        commit == "unknown"
            || (hash.len() == 40
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "output did not match the required version form: {stdout:?}"
    );
}
