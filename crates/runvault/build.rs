//! Records the toolchain that built the binary, so `run.json` can report it.
//!
//! Shelling out to `rustc --version` at run time would report whatever is
//! installed on the machine at that moment, which is not what produced the run.

use std::process::Command;

fn main() {
    let version = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    println!("cargo::rustc-env=RUNVAULT_RUSTC_VERSION={version}");
    println!("cargo::rerun-if-changed=build.rs");
}
