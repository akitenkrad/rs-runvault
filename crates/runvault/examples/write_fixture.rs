//! Writes the Rust-written run that the Python tests read back.
//!
//! `schema/v1/testvectors/` pins the bytes two implementations hash; it says
//! nothing about the directory they produce. This fixture pins the other half —
//! file layout, CSV headers, `status.json` — in the direction the vectors
//! cannot cover, so that a Python reader is checked against a run Rust actually
//! wrote rather than against one Python wrote for itself.
//!
//! Regenerate deliberately, never to make a test pass:
//!
//!     cargo run -p runvault --example write_fixture
//!
//! The run carries a fresh ULID and timestamp each time, so the output is not
//! byte-stable and is not diffed in CI.

use std::path::{Path, PathBuf};
use std::process::Command;

use runvault::meta::{Dataset, Target, Visibility, Work};
use runvault::{Run, RunOptions};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "python",
        "tests",
        "fixtures",
    ]
    .iter()
    .collect();
    let repo = out.join("rust_run");
    if repo.exists() {
        std::fs::remove_dir_all(&repo)?;
    }
    std::fs::create_dir_all(&repo)?;

    // A run records the commit it ran at, so the fixture needs a repository of
    // its own. Nothing about the host's checkout leaks into the committed files.
    git(&repo, &["init", "-q"])?;
    git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
    git(&repo, &["config", "user.name", "fixture"])?;
    std::fs::write(repo.join("Cargo.lock"), "# lock\n")?;
    std::fs::write(repo.join("uv.lock"), "version = 1\n")?;
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-qm", "fixture"])?;

    let cfg = json!({
        "rows": 13, "cols": 16, "threshold": 0.5,
        "seed": 42, "threads": 8, "log_level": "info",
        "output_dir": "results/whatever"
    });

    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id("runvault-fixture")
            .domain("simulation")
            .results_root(repo.join("results"))
            .repo_root(&repo)
            .parameters(&cfg)?
            .hash_exclude(["/output_dir", "/log_level"])
            .seed_pointers(["/seed"])
            .invariant_to(["/threads"])
            .master_seed(42)
            .data([Dataset::init("schelling-grid").dataset_id("schelling-grid@1")])
            .replication(
                Work::doi("10.1080/0022250X.1971.9989794")
                    .title("Dynamic Models of Segregation")
                    .source_version("published")
                    .target(Target::table("tbl3-r2", "Table 3").row("2"))
                    .obsidian_note("研究/98_論文レポート/80-再現実験/P00000009/設計書.md"),
            )
            .visibility(Visibility::Internal),
    )?;

    let dir = run.dir().to_path_buf();
    std::fs::create_dir_all(dir.join("artifacts"))?;
    std::fs::write(dir.join("artifacts/grid.svg"), "<svg/>\n")?;
    std::fs::create_dir_all(dir.join("logs"))?;
    std::fs::write(dir.join("logs/stdout.log"), "started\n")?;

    run.log_metric("segregation_index", 0.412)
        .step(1, "step")
        .scope("run")
        .send()?;
    run.log_metric("segregation_index", 0.834)
        .step(120, "step")
        .scope("run")
        .send()?;
    run.log_metric("n_units", 208.0).send()?;
    run.log_reference("segregation_index", 0.850)
        .scope("run")
        .target("tbl3-r2")
        .source("Table 3 row 2")
        .send()?;
    run.log_event(
        "observation",
        &json!({"unit_id": "a0042", "t": 3, "t_unit": "step", "moved": true}),
    )?;
    run.log_event(
        "terminal",
        &json!({"unit_id": "a0042", "t": 3, "t_unit": "step",
                "outcome": "settled", "censored": false, "budget": 100}),
    )?;
    run.finish()?;

    // The .git directory is the generator's scaffolding, not part of the run.
    std::fs::remove_dir_all(repo.join(".git"))?;
    println!("wrote {}", dir.display());
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let out = Command::new("git").current_dir(repo).args(args).output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(())
}
