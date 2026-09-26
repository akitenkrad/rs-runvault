use std::env;
use std::path::PathBuf;
use std::process::Command;

fn valid_commit(value: &str) -> bool {
    let hash = value.strip_suffix("-dirty").unwrap_or(value);
    value == "unknown"
        || (hash.len() == 40
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn source_commit() -> String {
    let Some(head) = git_output(&["rev-parse", "HEAD"]) else {
        return "unknown".to_owned();
    };
    if !valid_commit(&head) || head == "unknown" || head.ends_with("-dirty") {
        return "unknown".to_owned();
    }

    let Some(status) = git_output(&["status", "--porcelain", "--untracked-files=no"]) else {
        return "unknown".to_owned();
    };
    if status.is_empty() {
        head
    } else {
        format!("{head}-dirty")
    }
}

fn watch_git_path(name: &str) {
    if let Some(path) = git_output(&["rev-parse", "--git-path", name]) {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            env::current_dir().unwrap_or_default().join(path)
        };
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=RUNVAULT_GIT_COMMIT");
    watch_git_path("HEAD");
    watch_git_path("index");
    if let Some(reference) = git_output(&["symbolic-ref", "HEAD"]) {
        watch_git_path(&reference);
    }

    let commit = match env::var("RUNVAULT_GIT_COMMIT") {
        Ok(value) if valid_commit(&value) => value,
        Ok(_) => "unknown".to_owned(),
        Err(_) => source_commit(),
    };
    println!("cargo:rustc-env=RUNVAULT_GIT_COMMIT={commit}");
}
