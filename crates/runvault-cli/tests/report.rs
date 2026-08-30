//! `runvault report --obsidian`: the dashboard payload.
//!
//! Every payload produced here is checked against `schema/v1/runs.report.json`,
//! because that file is the contract with the Emera component and nothing else
//! sits between the two.

use std::path::{Path, PathBuf};
use std::process::Command;

use jsonschema::{Retrieve, Uri, Validator};
use runvault::meta::{Origin, Target, Visibility, Work};
use runvault::sync::{self, Planned, SyncOptions};
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

const REPO_ID: &str = "social-simulation-replications";

fn schema_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", "schema", "v1"]
        .iter()
        .collect()
}

struct SchemaFiles;

impl Retrieve for SchemaFiles {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let name = uri.path().as_str().rsplit('/').next().unwrap_or_default();
        Ok(serde_json::from_str(&std::fs::read_to_string(
            schema_dir().join(name),
        )?)?)
    }
}

#[track_caller]
fn assert_valid(instance: &Value) {
    let text = std::fs::read_to_string(schema_dir().join("runs.report.json")).unwrap();
    let validator: Validator = jsonschema::options()
        .should_validate_formats(true)
        .with_retriever(SchemaFiles)
        .build(&serde_json::from_str(&text).unwrap())
        .expect("schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{e}"))
        .collect();
    assert!(
        errors.is_empty(),
        "runs.report.json rejected: {}",
        errors.join("; ")
    );
}

fn private_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(sync::VAULT_CONFIG),
        "schema_version = \"1.0\"\nvisibility = \"private\"\n",
    )
    .unwrap();
    dir
}

/// A replication run with a paper, a target, a reported value and metrics.
fn replication_run(results: &Path, seed: u64, dirty_metric: f64) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .parameters(&json!({"rows": 13, "seed": seed}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(seed)
            .replication(
                Work::doi("10.1080/0022250X.1971.9989794")
                    .title("Dynamic Models of Segregation")
                    .source_version("published")
                    .target(Target::table("tbl3-r2", "Table 3").row("2"))
                    .obsidian_note("研究/98_論文レポート/80-再現実験/P00000009/設計書.md")
                    .jira("MYTASK-3058"),
            ),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.log_metric("segregation_index", dirty_metric)
        .send()
        .unwrap();
    // Reserved names appear in nearly every run; the dashboard must not show
    // them as an experiment's headline numbers.
    run.log_metric("n_units", 208.0).send().unwrap();
    run.log_metric("cost_usd", 0.25).send().unwrap();
    run.log_reference("segregation_index", 0.90)
        .target("tbl3-r2")
        .source("paper")
        .send()
        .unwrap();
    run.finish().unwrap();
    dir
}

/// A run directory in the shape used before this specification, with no experiment.
fn legacy_run_at_the_root(results: &Path, name: &str) -> PathBuf {
    let dir = results.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("metrics.csv"), "t,regions\n0,120\n10,44\n").unwrap();
    dir
}

fn sync_all(results: &Path, vault: &Path) {
    let options = SyncOptions {
        allow_internal: true,
        compress_over_bytes: 10 * 1024 * 1024,
    };
    for planned in sync::plan_all(results, REPO_ID, vault, &options).unwrap() {
        match planned {
            Planned::Send(plan) => {
                sync::execute(&plan).unwrap();
            }
            Planned::Skipped { run_dir, reason } => {
                panic!("{} が送られませんでした: {reason}", run_dir.display())
            }
        }
    }
}

fn runvault(vault: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .args(args)
        .args(["--vault", &vault.to_string_lossy()])
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Syncs, indexes and reports, returning the payload.
fn payload(results: &Path, vault: &Path) -> Value {
    sync_all(results, vault);
    assert!(runvault(vault, &["query", "--refresh"]).0);
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .args(["report", "--obsidian"])
        .args(["--vault", &vault.to_string_lossy()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("the report is JSON")
}

#[test]
fn the_payload_matches_the_contract_the_dashboard_reads() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    legacy_run_at_the_root(results.path(), "main_20240115_101500");
    let report = payload(results.path(), vault.path());

    assert_valid(&report);
    assert_eq!(report["schema_version"], json!("1.0"));
    assert!(report["vocab_version"].as_str().is_some());
    assert_eq!(report["freshness_hours"], json!(24.0));
}

#[test]
fn a_legacy_run_appears_without_being_given_a_state_it_never_recorded() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    legacy_run_at_the_root(results.path(), "main_20240115_101500");
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let runs = report["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    // `unfinished` means "written against this specification and holding no
    // status.json". A run from 2024 that completed is not that.
    assert_eq!(runs[0]["state"], Value::Null);
    // legacy でも置き場は引けなければならない．
    assert_eq!(runs[0]["repo_id"], json!(REPO_ID));
    assert_eq!(runs[0]["experiment"], Value::Null);
    assert_eq!(runs[0]["run_uid"], Value::Null);

    let experiments = report["experiments"].as_array().unwrap();
    assert_eq!(experiments.len(), 1);
    assert_eq!(experiments[0]["experiment"], Value::Null);
    assert_eq!(experiments[0]["repo_id"], json!(REPO_ID));
    assert_eq!(experiments[0]["n_runs"], json!(1));
    assert_eq!(experiments[0]["n_finished"], json!(0));

    let warnings = report["warnings"].as_array().unwrap();
    let legacy = warnings
        .iter()
        .find(|w| w["kind"] == json!("legacy_runs"))
        .expect("legacy runs are said out loud");
    assert_eq!(legacy["n_run"], json!(1));
}

#[test]
fn the_headline_metrics_are_results_rather_than_bookkeeping() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let experiments = report["experiments"].as_array().unwrap();
    let primary = experiments[0]["primary_metrics"].as_array().unwrap();
    // `n_units` and `cost_usd` occur in every run, so ranking by frequency
    // without excluding the registry's own names would show them and never the
    // number the experiment was run to produce.
    assert_eq!(primary, &[json!("segregation_index")], "{primary:?}");
    assert_eq!(experiments[0]["cost_usd"], json!(0.25));
    // origin は run.json にしか無い．`Origin::Manual` の run は code を持たないので
    // ここでは null になり，**埋められない**ことがそのまま出る．
    assert_eq!(experiments[0]["git_remote"], Value::Null);
}

#[test]
fn a_replication_carries_its_paper_and_the_gap_from_the_reported_value() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let run = &report["runs"].as_array().unwrap()[0];
    assert_eq!(
        run["replication"]["title"],
        json!("Dynamic Models of Segregation")
    );
    assert_eq!(run["replication"]["targets"], json!(["Table 3"]));
    assert_eq!(run["jira"], json!(["MYTASK-3058"]));
    // 画面はこれを使って集約先の run ディレクトリを引き，条件とハッシュを出す．
    assert_eq!(run["repo_id"], json!(REPO_ID));
    assert!(run["obsidian_note"].as_str().is_some());

    // 0.83 reproduced against 0.90 reported.
    let diff = run["replication"]["diff"]["segregation_index"]
        .as_f64()
        .unwrap();
    assert!((diff + 0.07).abs() < 1e-9, "{diff}");
    assert_eq!(run["metrics"]["segregation_index"], json!(0.83));
}

/// The same run, recorded against a different interpreter.
///
/// `python_version` is part of `env_hash` and not of `config_hash`, so this is
/// a genuine environment split rather than an edited file — which could not
/// reach the aggregation repository at all, since `sync` verifies first.
fn run_under_another_interpreter(results: &Path, seed: u64) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .parameters(&json!({"rows": 13, "seed": seed}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(seed)
            .python_version("3.13.1"),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.log_metric("segregation_index", 0.84).send().unwrap();
    run.finish().unwrap();
    dir
}

#[test]
fn two_environments_for_one_condition_are_said_out_loud() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let a = plain_run(results.path(), 42);
    let b = run_under_another_interpreter(results.path(), 42);
    assert_ne!(a, b);

    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    // Two runs that a table would show side by side, produced by different
    // toolchains. The condition is the same, so the numbers look comparable.
    let warnings = report["warnings"].as_array().unwrap();
    let split = warnings
        .iter()
        .find(|w| w["kind"] == json!("env_split"))
        .unwrap_or_else(|| panic!("環境の割れが報告されていません: {warnings:?}"));
    assert_eq!(split["n_run"], json!(2));
    assert!(split["config_hash"].as_str().is_some());
}

/// The same condition as `run_under_another_interpreter`, on this toolchain.
fn plain_run(results: &Path, seed: u64) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .parameters(&json!({"rows": 13, "seed": seed}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(seed),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.log_metric("segregation_index", 0.83).send().unwrap();
    run.finish().unwrap();
    dir
}

#[test]
fn the_report_is_written_whole_or_not_at_all() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    sync_all(results.path(), vault.path());
    assert!(runvault(vault.path(), &["query", "--refresh"]).0);

    let out = vault.path().join("_data").join("runs.json");
    let written = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .args(["report", "--obsidian"])
        .args(["--vault", &vault.path().to_string_lossy()])
        .args(["--out", &out.to_string_lossy()])
        .output()
        .unwrap();
    assert!(written.status.success());

    // The directory is created on the way, and the file parses.
    let payload: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_valid(&payload);
}

#[test]
fn asking_for_no_destination_is_an_error() {
    let vault = private_vault();
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .arg("report")
        .args(["--vault", &vault.path().to_string_lossy()])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
