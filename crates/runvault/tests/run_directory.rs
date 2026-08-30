//! End-to-end: a run directory is produced, and every file in it is checked
//! against the frozen schemas in `schema/v1/`.
//!
//! The schemas decide whether a file *can* be written that way; `runvault
//! verify` decides whether the run contradicts itself. Both are exercised here,
//! because either alone lets a well-formed but inconsistent run through.

use std::path::{Path, PathBuf};
use std::process::Command;

use jsonschema::{Retrieve, Uri, Validator};
use runvault::meta::{Dataset, Llm, Origin, Target, Visibility, Work};
use runvault::status::{RunStatus, State};
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

fn schema_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", "schema", "v1"]
        .iter()
        .collect()
}

fn load_schema(name: &str) -> Value {
    let path = schema_dir().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("schema is valid JSON")
}

/// Serves the frozen schemas from disk, so the relative `$ref`s to
/// `common.json` resolve to the files rather than to the network.
struct SchemaFiles;

impl Retrieve for SchemaFiles {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let name = uri.path().as_str().rsplit('/').next().unwrap_or_default();
        let path = schema_dir().join(name);
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }
}

/// Builds a validator that resolves every reference against `schema/v1/`.
fn validator(name: &str) -> Validator {
    jsonschema::options()
        .should_validate_formats(true)
        .with_retriever(SchemaFiles)
        .build(&load_schema(name))
        .expect("schema compiles")
}

#[track_caller]
fn assert_valid(name: &str, instance: &Value) {
    let validator = validator(name);
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("{e}"))
        .collect();
    assert!(
        errors.is_empty(),
        "{name}.json rejected {}: {}",
        serde_json::to_string(instance).unwrap(),
        errors.join("; ")
    );
}

/// Turns a CSV row into the JSON object its row schema describes.
///
/// The columns are typed by the schema, not by the file: an empty field is a
/// `null`, never the string `""`.
fn csv_rows(path: &Path, numeric: &[&str], integral: &[&str]) -> Vec<Value> {
    let mut reader = csv::Reader::from_path(path).expect("csv opens");
    let header: Vec<String> = reader
        .headers()
        .unwrap()
        .iter()
        .map(str::to_string)
        .collect();
    reader
        .records()
        .map(|record| {
            let record = record.unwrap();
            let mut object = serde_json::Map::new();
            for (name, field) in header.iter().zip(record.iter()) {
                let value = if field.is_empty() {
                    Value::Null
                } else if integral.contains(&name.as_str()) {
                    json!(field.parse::<i64>().expect("integer column"))
                } else if numeric.contains(&name.as_str()) {
                    json!(field.parse::<f64>().expect("numeric column"))
                } else {
                    Value::String(field.to_string())
                };
                object.insert(name.clone(), value);
            }
            Value::Object(object)
        })
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .expect("valid JSON")
}

/// A git repository with one commit, so `origin = code` has something to record.
fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@example.com"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(dir.path().join("Cargo.lock"), "# lock").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-qm", "first"]);
    dir
}

fn simulation_options(repo: &Path, results: &Path, cfg: &Value) -> RunOptions {
    RunOptions::new("schelling", "main")
        .repo_id("social-simulation-replications")
        .domain("simulation")
        .results_root(results)
        .repo_root(repo)
        .parameters(cfg)
        .unwrap()
        .hash_exclude(["/output_dir", "/log_level"])
        .seed_pointers(["/seed"])
        .master_seed(42)
}

#[test]
fn a_simulation_run_produces_a_directory_every_schema_accepts() {
    let repo = git_repo();
    let results = tempfile::tempdir().unwrap();
    let cfg = json!({
        "rows": 13, "cols": 16, "threshold": 0.5,
        "seed": 42, "log_level": "info", "output_dir": "results/whatever"
    });

    let mut run = Run::start(simulation_options(repo.path(), results.path(), &cfg)).unwrap();
    let dir = run.dir().to_path_buf();

    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts/grid.svg"), "<svg/>").unwrap();
    std::fs::create_dir_all(dir.join("logs")).unwrap();
    std::fs::write(dir.join("logs/stdout.log"), "started\n").unwrap();

    run.log_metric("segregation_index", 0.834)
        .step(120, "step")
        .scope("run")
        .send()
        .unwrap();
    run.log_metric("segregation_index", 0.412)
        .step(1, "step")
        .scope("run")
        .send()
        .unwrap();
    run.log_metric("n_units", 208.0).send().unwrap();
    run.log_event(
        "observation",
        &json!({"unit_id": "a0042", "t": 3, "t_unit": "step", "moved": true}),
    )
    .unwrap();
    run.log_event(
        "terminal",
        &json!({"unit_id": "a0042", "t": 3, "t_unit": "step",
                "outcome": "settled", "censored": false, "budget": 500}),
    )
    .unwrap();
    let finished = run.finish().unwrap();
    assert_eq!(finished, dir);

    assert_valid("run", &read_json(&dir.join("run.json")));
    assert_valid("config", &read_json(&dir.join("config.json")));
    assert_valid("status", &read_json(&dir.join("status.json")));

    for row in csv_rows(&dir.join("metrics.csv"), &["value"], &["step"]) {
        assert_valid("metrics.row", &row);
    }
    for row in csv_rows(&dir.join("manifest.csv"), &[], &["bytes"]) {
        assert_valid("manifest.row", &row);
    }
    for line in std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
    {
        assert_valid("event", &serde_json::from_str::<Value>(line).unwrap());
    }

    // The lock file is gone, the manifest covers both trees, and the link points here.
    assert!(!dir.join(".runvault.lock").exists());
    let manifest = std::fs::read_to_string(dir.join("manifest.csv")).unwrap();
    assert!(manifest.contains("artifacts/grid.svg"), "{manifest}");
    assert!(manifest.contains("logs/stdout.log"), "{manifest}");
    assert!(dir.join("lock/Cargo.lock").is_file());

    let link = results.path().join("schelling/latest_finished");
    assert_eq!(
        std::fs::read_link(&link).unwrap().to_string_lossy(),
        *run_slug(&dir)
    );

    let status: RunStatus = serde_json::from_value(read_json(&dir.join("status.json"))).unwrap();
    assert_eq!(status.state, State::Finished);
    assert_eq!(status.exit_code, Some(0));
    assert_eq!(status.counts.unwrap().metrics, 3);
}

fn run_slug(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().to_string()
}

#[test]
fn a_replication_records_the_paper_and_its_reported_values() {
    let repo = git_repo();
    let results = tempfile::tempdir().unwrap();
    let cfg = json!({"rows": 13, "seed": 1});

    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id("social-simulation-replications")
            .domain("simulation")
            .results_root(results.path())
            .repo_root(repo.path())
            .parameters(&cfg)
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(1)
            .data([Dataset::init("schelling-grid").dataset_id("grid@v1").n(208)])
            .replication(
                Work::doi("10.1080/0022250X.1971.9989794")
                    .title("Dynamic Models of Segregation")
                    .source_version("published")
                    .target(
                        Target::table("tbl3-r2", "Table 3")
                            .row("2")
                            .condition("threshold=0.5"),
                    )
                    .obsidian_note("研究/98_論文レポート/80-再現実験/P00000009/設計書.md")
                    .jira("MYTASK-3054"),
            )
            .visibility(Visibility::Internal),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();

    run.log_metric("segregation_index", 0.834).send().unwrap();
    run.log_reference("segregation_index", 0.850)
        .target("tbl3-r2")
        .source("Table 3 row 2")
        .send()
        .unwrap();
    run.finish().unwrap();

    assert_valid("run", &read_json(&dir.join("run.json")));
    for row in csv_rows(&dir.join("reference.csv"), &["value"], &["step"]) {
        assert_valid("reference.row", &row);
    }
}

#[test]
fn a_manual_run_needs_no_repository_but_still_records_its_environment() {
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("annotation", "label")
            .repo_id("hand-annotation")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"guideline": "v3"}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.finish().unwrap();

    let meta = read_json(&dir.join("run.json"));
    assert_valid("run", &meta);
    assert!(meta["code"].is_null());
    assert!(!meta["env"]["env_hash"].as_str().unwrap().is_empty());
}

#[test]
fn an_llm_run_records_the_model_snapshot() {
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("multiturn-jailbreak-budget", "sweep")
            .repo_id("rs-jailbreak-bench")
            .domain("llm-safety")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"budget": 10}))
            .unwrap()
            .llm(Llm {
                provider: "anthropic".into(),
                model_snapshot: "claude-opus-5-20260401".into(),
                temperature: Some(1.0),
                system_prompt_hash: None,
            }),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.finish().unwrap();
    assert_valid("run", &read_json(&dir.join("run.json")));
}

#[test]
fn a_parallel_sweep_of_the_same_condition_does_not_share_a_directory() {
    // Same condition, same seed, started back to back: the condition prefix is
    // identical, so only the identifiers keep these two runs apart.
    let repo = git_repo();
    let results = tempfile::tempdir().unwrap();
    let cfg = json!({"rows": 13, "seed": 7, "log_level": "info", "output_dir": "x"});

    let first = Run::start(simulation_options(repo.path(), results.path(), &cfg)).unwrap();
    let second = Run::start(simulation_options(repo.path(), results.path(), &cfg)).unwrap();
    assert_ne!(first.dir(), second.dir());
    assert_ne!(first.run_uid(), second.run_uid());

    let first_meta = read_json(&first.dir().join("run.json"));
    let second_meta = read_json(&second.dir().join("run.json"));
    assert_eq!(first_meta["config_hash"], second_meta["config_hash"]);
    assert_eq!(first_meta["execution_hash"], second_meta["execution_hash"]);

    second.finish().unwrap();
    first.finish().unwrap();
}

#[test]
fn a_status_that_records_a_collision_is_still_a_valid_status() {
    // The suffix itself is exercised in the unit test for `create_run_dir`;
    // what matters here is that the number it produces survives the schema.
    let status = json!({
        "schema_version": "1.0",
        "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X4Z",
        "state": "finished",
        "started_at": "2026-08-30T10:15:00+09:00",
        "finished_at": "2026-08-30T10:15:31+09:00",
        "duration_sec": 31.0,
        "exit_code": 0,
        "collision_index": 2,
        "error": null,
        "counts": {"metrics": 3, "events": 2, "artifacts": 1}
    });
    assert_valid("status", &status);
}

#[test]
fn dropping_a_run_without_finishing_records_a_failure() {
    let results = tempfile::tempdir().unwrap();
    let dir = {
        let run = Run::start(
            RunOptions::new("schelling", "main")
                .repo_id("r")
                .domain("other")
                .origin(Origin::Manual)
                .results_root(results.path())
                .parameters(&json!({"a": 1}))
                .unwrap(),
        )
        .unwrap();
        run.dir().to_path_buf()
    };

    let status = read_json(&dir.join("status.json"));
    assert_valid("status", &status);
    assert_eq!(status["state"], "failed");
    assert_eq!(status["error"]["kind"], "dropped");
    assert!(!dir.join(".runvault.lock").exists());
    // A failed run must not become the experiment's latest.
    assert!(!results.path().join("schelling/latest_finished").exists());
}

#[test]
fn verify_catches_a_metrics_file_belonging_to_another_run() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.log_metric("asr", 0.21).send().unwrap();
    run.finish().unwrap();
    assert!(runvault::verify::shallow(&dir).is_ok());

    let metrics = std::fs::read_to_string(dir.join("metrics.csv")).unwrap();
    std::fs::write(
        dir.join("metrics.csv"),
        metrics.replace(run_uid(&dir).as_str(), "01K3QZ8F7H9M2N4P6R8T0V2X4Z"),
    )
    .unwrap();

    let err = runvault::verify::shallow(&dir).unwrap_err().to_string();
    assert!(err.contains("metrics.csv"), "{err}");
}

#[test]
fn verify_catches_a_directory_renamed_away_from_its_hashes() {
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.finish().unwrap();

    let renamed = dir
        .parent()
        .unwrap()
        .join("main_20200101_000000_deadbeef_0000");
    std::fs::rename(&dir, &renamed).unwrap();
    let err = runvault::verify::shallow(&renamed).unwrap_err().to_string();
    assert!(err.contains("run_slug"), "{err}");
}

#[test]
fn a_left_over_lock_makes_a_finished_run_look_running() {
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.finish().unwrap();

    std::fs::write(dir.join(".runvault.lock"), "{}").unwrap();
    let err = runvault::verify::shallow(&dir).unwrap_err().to_string();
    assert!(err.contains(".runvault.lock"), "{err}");
}

fn run_uid(dir: &Path) -> String {
    read_json(&dir.join("run.json"))["run_uid"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_simulation_without_a_seed_is_refused_before_anything_is_written() {
    let results = tempfile::tempdir().unwrap();
    let err = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("simulation")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .err()
    .map(|e| e.to_string())
    .expect("start should have refused");
    assert!(err.contains("master_seed"), "{err}");
    assert!(
        !results.path().join("e").exists(),
        "nothing should have been created"
    );
}

#[test]
fn an_exclusion_that_points_nowhere_is_refused() {
    let results = tempfile::tempdir().unwrap();
    let err = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"rows": 13}))
            .unwrap()
            .hash_exclude(["/output_dir"]),
    )
    .err()
    .map(|e| e.to_string())
    .expect("start should have refused");
    assert!(err.contains("/output_dir"), "{err}");
}

#[test]
fn a_terminal_event_that_is_terminal_in_name_only_is_refused() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();

    let err = run
        .log_event(
            "terminal",
            &json!({"unit_id": "t1", "t": 3, "t_unit": "turn"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("outcome"), "{err}");
    assert!(err.contains("budget"), "{err}");

    // An experiment's own event kind is not constrained that way.
    run.log_event("x.r.flow_summary", &json!({"flow": 12}))
        .unwrap();
    run.finish().unwrap();
}

#[test]
fn a_reserved_metric_at_the_wrong_scope_is_refused() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let err = run
        .log_metric("cost_usd", 3.42)
        .scope("trial")
        .send()
        .unwrap_err()
        .to_string();
    assert!(err.contains("cost_usd"), "{err}");
    run.log_metric("cost_usd", 3.42).send().unwrap();
    run.finish().unwrap();
}

#[test]
fn a_reported_value_must_name_the_target_it_came_from() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap()
            .replication(
                Work::arxiv("2301.12345")
                    .title("A Paper")
                    .source_version("arXiv v2")
                    .target(Target::figure("fig2a", "Figure 2").panel("a"))
                    .obsidian_note("note.md"),
            ),
    )
    .unwrap();

    assert!(run.log_reference("asr", 0.5).send().is_err());
    assert!(
        run.log_reference("asr", 0.5)
            .target("nope")
            .source("Fig 2")
            .send()
            .is_err()
    );
    run.log_reference("asr", 0.5)
        .target("fig2a")
        .source("Figure 2 panel a")
        .send()
        .unwrap();
    run.finish().unwrap();
}

#[test]
fn a_dirty_working_tree_is_recorded_with_the_hash_of_its_difference() {
    let repo = git_repo();
    std::fs::write(repo.path().join("scratch.rs"), "fn main() {}").unwrap();
    let results = tempfile::tempdir().unwrap();
    let cfg = json!({"rows": 1, "seed": 1});

    let run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .results_root(results.path())
            .repo_root(repo.path())
            .parameters(&cfg)
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.finish().unwrap();

    let meta = read_json(&dir.join("run.json"));
    assert_valid("run", &meta);
    assert_eq!(meta["code"]["git_dirty"], true);
    assert_eq!(meta["code"]["dirty_hash"]["algorithm"], "blake3");
}

#[test]
fn gc_turns_a_killed_run_into_a_recorded_failure() {
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    // Simulate SIGKILL: the process is gone, the lock stayed, `Drop` never ran.
    std::mem::forget(run);

    let mut record: Value = read_json(&dir.join(".runvault.lock"));
    record["pid"] = json!(0);
    record["process_start_time"] = json!(1);
    record["heartbeat_at"] =
        json!((chrono::Local::now() - chrono::Duration::hours(2)).to_rfc3339());
    std::fs::write(
        dir.join(".runvault.lock"),
        serde_json::to_string(&record).unwrap(),
    )
    .unwrap();

    let reaped = runvault::gc::collect(results.path(), false).unwrap();
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].outcome, runvault::gc::Outcome::Reaped);

    let status = read_json(&dir.join("status.json"));
    assert_valid("status", &status);
    assert_eq!(status["state"], "failed");
    assert_eq!(status["error"]["kind"], "killed");
    assert!(!dir.join(".runvault.lock").exists());
}

#[test]
fn the_cli_finds_verifies_and_sweeps_runs() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id("r")
            .domain("simulation")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"rows": 13, "seed": 3}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(3),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    let config_hash = read_json(&dir.join("run.json"))["config_hash"]
        .as_str()
        .unwrap()
        .to_string();
    run.log_metric("segregation_index", 0.7).send().unwrap();
    run.finish().unwrap();

    let runvault = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let root = results.path().to_string_lossy().to_string();

    let (ok, stdout) = runvault(&[
        "path",
        "--results-root",
        &root,
        "--experiment",
        "schelling",
        "--latest",
    ]);
    assert!(ok, "{stdout}");
    assert!(stdout.trim().ends_with(run_slug(&dir).as_str()), "{stdout}");

    let (ok, stdout) = runvault(&[
        "path",
        "--results-root",
        &root,
        "--experiment",
        "schelling",
        "--config-hash",
        &config_hash[..8],
    ]);
    assert!(ok, "{stdout}");
    assert_eq!(stdout.trim(), dir.to_string_lossy());

    // A prefix that matches nothing is a failure, so a shell script can branch on it.
    let (ok, _) = runvault(&[
        "path",
        "--results-root",
        &root,
        "--experiment",
        "schelling",
        "--config-hash",
        "ffffffff",
    ]);
    assert!(!ok);

    let (ok, stdout) = runvault(&["verify", &dir.to_string_lossy()]);
    assert!(ok, "{stdout}");

    let (ok, stdout) = runvault(&["gc", "--results-root", &root]);
    assert!(ok);
    assert!(stdout.contains("0 件が異常終了"), "{stdout}");
}

#[test]
fn an_explicit_failure_tears_down_in_the_same_order_as_a_success() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.log_metric("asr", 0.21).send().unwrap();
    assert!(
        dir.join(".runvault.lock").is_file(),
        "the run should be holding its lock"
    );

    let returned = run.fail("api", "the provider returned 500 twice").unwrap();
    assert_eq!(returned, dir);

    // The lock must be gone before status.json exists, or a finished run reads
    // as still running — the one state verify refuses.
    assert!(!dir.join(".runvault.lock").exists());
    let status = read_json(&dir.join("status.json"));
    assert_valid("status", &status);
    assert_eq!(status["state"], "failed");
    assert_eq!(status["error"]["kind"], "api");
    assert_eq!(status["counts"]["metrics"], 1);

    runvault::verify::shallow(&dir).expect("a failed run must still be self-consistent");

    // A failed run is not the experiment's latest.
    assert!(!results.path().join("e/latest_finished").exists());
}

#[test]
fn concurrent_finishes_leave_the_link_on_the_newest_one() {
    // Both runs read the link and both replace it; without a lock around the
    // compare-and-swap the one that finished first could install itself last.
    use std::sync::{Arc, Barrier};

    let results = tempfile::tempdir().unwrap();
    let experiment = results.path().join("race");
    std::fs::create_dir_all(&experiment).unwrap();

    let make = |slug: &str, finished_at: &str| {
        let dir = experiment.join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let status = json!({
            "schema_version": "1.0",
            "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X4Z",
            "state": "finished",
            "started_at": finished_at,
            "finished_at": finished_at,
            "duration_sec": 1.0,
            "exit_code": 0,
            "collision_index": null,
            "error": null,
            "counts": null
        });
        std::fs::write(
            dir.join("status.json"),
            serde_json::to_string(&status).unwrap(),
        )
        .unwrap();
    };
    make("early", "2026-08-30T09:00:00+09:00");
    make("late", "2026-08-30T12:00:00+09:00");

    for _ in 0..20 {
        let _ = std::fs::remove_file(experiment.join("latest_finished"));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = [
            ("late", "2026-08-30T12:00:00+09:00"),
            ("early", "2026-08-30T09:00:00+09:00"),
        ]
        .into_iter()
        .map(|(slug, at)| {
            let experiment = experiment.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                runvault::paths::update_latest_finished(&experiment, slug, at).unwrap();
            })
        })
        .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let target = std::fs::read_link(experiment.join("latest_finished")).unwrap();
        assert_eq!(
            target.to_string_lossy(),
            "late",
            "the link went back to the run that finished earlier"
        );
    }
}

#[test]
fn resuming_a_run_whose_status_cannot_be_read_is_refused() {
    let results = tempfile::tempdir().unwrap();
    let options = || {
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap()
    };

    let first = Run::start(options()).unwrap();
    let first_dir = first.dir().to_path_buf();
    let first_uid = first.run_uid().to_string();
    first.fail("crash", "died mid-run").unwrap();

    let second = Run::start(options().lineage(runvault::Lineage {
        sweep_id: None,
        parent_run_uid: None,
        resumed_from: Some(first_uid),
        derived_from: None,
    }))
    .unwrap();
    let second_dir = second.dir().to_path_buf();
    second.finish().unwrap();
    runvault::verify::shallow(&second_dir).expect("resuming a failed run is fine");

    // The run it resumes is right there, but no longer says it failed.
    std::fs::remove_file(first_dir.join("status.json")).unwrap();
    let err = runvault::verify::shallow(&second_dir)
        .unwrap_err()
        .to_string();
    assert!(err.contains("resumed_from"), "{err}");
}

#[test]
fn two_runs_that_name_each_other_as_a_sweep_parent_are_a_cycle() {
    let results = tempfile::tempdir().unwrap();
    let options = || {
        RunOptions::new("e", "sweep")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap()
    };

    let a = Run::start(options()).unwrap();
    let (a_dir, a_uid) = (a.dir().to_path_buf(), a.run_uid().to_string());
    a.finish().unwrap();
    let b = Run::start(options()).unwrap();
    let (b_dir, b_uid) = (b.dir().to_path_buf(), b.run_uid().to_string());
    b.finish().unwrap();

    // Point each at the other. A chain walk that only follows resume/derive
    // never notices, and an aggregate that walks parents never terminates.
    let point_at = |dir: &Path, uid: &str| {
        let mut meta = read_json(&dir.join("run.json"));
        meta["lineage"] = json!({
            "sweep_id": "s1",
            "parent_run_uid": uid,
            "resumed_from": null,
            "derived_from": null
        });
        assert_valid("run", &meta);
        std::fs::write(dir.join("run.json"), serde_json::to_string(&meta).unwrap()).unwrap();
    };
    point_at(&a_dir, &b_uid);
    point_at(&b_dir, &a_uid);

    let err = runvault::verify::shallow(&a_dir).unwrap_err().to_string();
    assert!(err.contains("循環"), "{err}");
}

#[test]
fn gc_never_leaves_a_live_run_holding_both_a_status_and_a_lock() {
    // gc reads the lock, judges it, then acts. Hammering a run that is alive the
    // whole time must never produce the one state verify refuses.
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();

    for _ in 0..50 {
        let swept = runvault::gc::collect(results.path(), false).unwrap();
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].outcome, runvault::gc::Outcome::Running);
        assert!(
            dir.join(".runvault.lock").is_file(),
            "the live run lost its lock"
        );
        assert!(
            !dir.join("status.json").exists(),
            "a live run was marked finished"
        );
    }
    run.finish().unwrap();
    runvault::verify::shallow(&dir).unwrap();
}

#[test]
fn the_cli_answers_whether_this_exact_thing_already_ran() {
    // A sweep asks this before starting each condition; a failed attempt must
    // not count as one that happened.
    let results = tempfile::tempdir().unwrap();
    let options = |seed: u64| {
        RunOptions::new("schelling", "main")
            .repo_id("r")
            .domain("simulation")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"rows": 13, "seed": seed}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(seed)
    };

    let done = Run::start(options(1)).unwrap();
    let done_meta = read_json(&done.dir().join("run.json"));
    let (config_hash, execution_hash) = (
        done_meta["config_hash"].as_str().unwrap().to_string(),
        done_meta["execution_hash"].as_str().unwrap().to_string(),
    );
    done.finish().unwrap();

    let attempted = Run::start(options(2)).unwrap();
    let attempted_execution = read_json(&attempted.dir().join("run.json"))["execution_hash"]
        .as_str()
        .unwrap()
        .to_string();
    attempted.fail("api", "died").unwrap();

    let runvault = |args: &[&str]| -> bool {
        Command::new(env!("CARGO_BIN_EXE_runvault"))
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    };
    let root = results.path().to_string_lossy().to_string();
    let ask = |hash: &str, finished: bool| {
        let mut args = vec![
            "path",
            "--results-root",
            &root,
            "--experiment",
            "schelling",
            "--execution-hash",
            hash,
        ];
        if finished {
            args.push("--finished");
        }
        runvault(&args)
    };

    assert!(
        ask(&execution_hash, true),
        "the finished run should be found"
    );
    assert!(
        !ask(&attempted_execution, true),
        "a failed attempt is not a run that happened"
    );
    assert!(
        ask(&attempted_execution, false),
        "without --finished it is still a run"
    );

    // The two runs are replicates: same condition, different execution.
    assert_ne!(execution_hash, attempted_execution);
    assert!(runvault(&[
        "path",
        "--results-root",
        &root,
        "--experiment",
        "schelling",
        "--config-hash",
        &config_hash,
    ]));
}

#[test]
fn a_sweep_parent_is_driven_by_a_list_of_seeds_not_by_one() {
    // Its children each have a seed; the parent has the list, which reaches
    // execution_hash through seed_pointers. Demanding one seed of the parent
    // would mean writing a representative value that is not true.
    let results = tempfile::tempdir().unwrap();
    let parent = Run::start(
        RunOptions::new("schelling", "sweep")
            .repo_id("schelling1971")
            .domain("simulation")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"threshold": {"start": 0.4, "stop": 0.6}, "seeds": [42, 43]}))
            .unwrap()
            .seed_pointers(["/seeds"])
            .lineage(runvault::Lineage {
                sweep_id: Some("sweep-a".into()),
                ..Default::default()
            }),
    )
    .unwrap();
    let (parent_dir, sweep_id, parent_uid) = (
        parent.dir().to_path_buf(),
        "sweep-a".to_string(),
        parent.run_uid().to_string(),
    );

    let meta = read_json(&parent_dir.join("run.json"));
    assert_valid("run", &meta);
    assert!(meta["rng"]["master_seed"].is_null());

    let mut child_dirs = Vec::new();
    for (index, seed) in [42u64, 43].into_iter().enumerate() {
        let child = Run::start(
            RunOptions::new("schelling", "run")
                .repo_id("schelling1971")
                .domain("simulation")
                .origin(Origin::Manual)
                .results_root(results.path())
                .parameters(&json!({"threshold": 0.5, "seed": seed}))
                .unwrap()
                .seed_pointers(["/seed"])
                .master_seed(seed)
                .replicate_index(index as u64)
                .lineage(runvault::Lineage {
                    sweep_id: Some(sweep_id.clone()),
                    parent_run_uid: Some(parent_uid.clone()),
                    ..Default::default()
                }),
        )
        .unwrap();
        child_dirs.push(child.dir().to_path_buf());
        child.finish().unwrap();
    }
    parent.finish().unwrap();

    for dir in &child_dirs {
        let meta = read_json(&dir.join("run.json"));
        assert_valid("run", &meta);
        assert_eq!(meta["lineage"]["parent_run_uid"], json!(parent_uid));
        assert!(
            !meta["rng"]["master_seed"].is_null(),
            "a child still needs its seed"
        );
    }
    // Same condition, different seed: the replicates share config_hash.
    let hash_of = |dir: &Path| read_json(&dir.join("run.json"))["config_hash"].clone();
    assert_eq!(hash_of(&child_dirs[0]), hash_of(&child_dirs[1]));

    // The parent finished last, so `--latest` alone hands back the run that
    // holds no metrics. Narrowing by subcommand is what a plotter needs.
    let runvault = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    };
    let root = results.path().to_string_lossy().to_string();
    let (ok, latest) = runvault(&[
        "path",
        "--results-root",
        &root,
        "--experiment",
        "schelling",
        "--latest",
    ]);
    assert!(ok);
    assert!(latest.contains("/sweep_"), "{latest}");

    let (ok, latest_run) = runvault(&[
        "path",
        "--results-root",
        &root,
        "--experiment",
        "schelling",
        "--latest",
        "--subcommand",
        "run",
    ]);
    assert!(ok, "{latest_run}");
    assert!(latest_run.contains("/run_"), "{latest_run}");
    assert_eq!(latest_run, child_dirs[1].to_string_lossy());
}

#[test]
fn metrics_written_as_a_batch_land_as_the_same_rows() {
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();

    run.log_metrics(Some((3, "step")), "run", &[("a", 0.5), ("b", 1.5)])
        .unwrap();
    run.log_metrics(None, "run", &[("asr", 0.25)]).unwrap();
    // The batch is checked as a whole before anything is written.
    assert!(
        run.log_metrics(None, "trial", &[("cost_usd", 1.0)])
            .is_err()
    );
    run.finish().unwrap();

    let rows = csv_rows(&dir.join("metrics.csv"), &["value"], &["step"]);
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_valid("metrics.row", row);
    }
    assert_eq!(rows[0]["name"], "a");
    assert_eq!(rows[0]["step"], 3);
    assert_eq!(rows[0]["step_unit"], "step");
    assert_eq!(rows[2]["name"], "asr");
    assert!(rows[2]["step"].is_null());
}
