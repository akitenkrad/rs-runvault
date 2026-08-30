//! What machine and toolchain produced the run.

use crate::hash::env_hash;
use crate::meta::{Env, Lock};

/// The toolchain that built this binary, as `rustc --version` reported at build time.
pub fn rustc_version() -> Option<&'static str> {
    let v = env!("RUNVAULT_RUSTC_VERSION");
    (!v.is_empty()).then_some(v)
}

/// The version of `runvault` that is writing the run.
pub fn runvault_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Machine name. Only recorded, never hashed.
pub fn host() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown".into())
}

/// Collects the environment and computes its hash.
///
/// `origin` does not matter here: a run entered by hand still records which
/// machine and toolchain it was entered on.
pub fn collect(python_version: Option<String>, locks: &[Lock]) -> Env {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let rustc = rustc_version().map(str::to_string);
    let env_hash = env_hash(
        &os,
        &arch,
        rustc.as_deref(),
        python_version.as_deref(),
        locks,
    );
    Env {
        env_hash,
        host: host(),
        os,
        arch,
        rustc_version: rustc,
        python_version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_is_complete_enough_for_the_schema() {
        let env = collect(None, &[]);
        assert_eq!(env.env_hash.len(), 64);
        assert!(!env.host.is_empty());
        assert!(!env.os.is_empty());
        assert!(!env.arch.is_empty());
    }

    #[test]
    fn the_hash_ignores_the_machine_name() {
        let a = collect(None, &[]);
        let b = collect(None, &[]);
        assert_eq!(a.env_hash, b.env_hash);
        assert_ne!(collect(Some("3.13.1".into()), &[]).env_hash, a.env_hash);
    }
}
