//! The command line, exercised as a caller would: by running the binary.
//!
//! These tests live in the CLI crate because that is where the binary is, and
//! the binary is here rather than in `runvault` because the index bundles
//! DuckDB and the replication repositories must not compile a database in
//! order to write a run.

use std::path::{Path, PathBuf};
use std::process::Command;

use jsonschema::{Retrieve, Uri, Validator};
use runvault::meta::{Origin, Visibility};
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

fn schema_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", "schema", "v1"]
        .iter()
        .collect()
}

/// Serves `schema/v1/` from disk so the relative `$ref`s resolve to the files.
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
fn assert_valid(name: &str, instance: &Value) {
    let text = std::fs::read_to_string(schema_dir().join(format!("{name}.json"))).unwrap();
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
        "{name}.json rejected: {}",
        errors.join("; ")
    );
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const REPO_ID: &str = "social-simulation-replications";

/// An aggregation repository that declares itself private.
fn private_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(runvault::sync::VAULT_CONFIG),
        "schema_version = \"1.0\"\nvisibility = \"private\"\n",
    )
    .unwrap();
    dir
}

/// A finished run with an artifact, so `manifest.csv` has something to cover.
fn finished_run(results: &Path) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .parameters(&json!({"rows": 13, "seed": 42}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(42),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts/figure.png"), vec![0u8; 4096]).unwrap();
    run.log_metric("segregation_index", 0.83).send().unwrap();
    run.finish().unwrap();
    dir
}

fn run_slug(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().to_string()
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
    // The command prints the path as the filesystem sees it, so the expected
    // side has to be resolved too: on macOS `TMPDIR` is a symlink.
    assert_eq!(
        stdout.trim(),
        std::fs::canonicalize(&dir).unwrap().to_string_lossy()
    );

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
fn the_cli_asks_for_the_deep_checks_only_when_told_to() {
    let results = tempfile::tempdir().unwrap();
    let dir = finished_run(results.path());
    let mut config = read_json(&dir.join("config.json"));
    config["parameters"]["rows"] = json!(14);
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    let verify = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_runvault"))
            .arg("verify")
            .arg(&dir)
            .args(args)
            .output()
            .unwrap()
    };

    assert!(verify(&[]).status.success(), "shallow は通るはずでした");
    let deep = verify(&["--deep"]);
    assert!(!deep.status.success());
    assert!(
        String::from_utf8_lossy(&deep.stderr).contains("config_hash"),
        "{}",
        String::from_utf8_lossy(&deep.stderr)
    );
}

#[test]
fn the_command_shows_what_would_enter_the_repository_before_it_does() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    finished_run(results.path());

    let run_cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_runvault"))
            .arg("sync")
            .args(["--results-root", &results.path().to_string_lossy()])
            .args(["--repo-id", REPO_ID])
            .args(["--vault", &vault.path().to_string_lossy()])
            .args(args)
            .output()
            .unwrap()
    };

    let dry = run_cli(&["--dry-run"]);
    let stdout = String::from_utf8_lossy(&dry.stdout).to_string();
    assert!(dry.status.success(), "{stdout}");
    assert!(stdout.contains("would send"), "{stdout}");
    assert!(stdout.contains("run.json"), "{stdout}");
    // A dry run writes nothing at all, or it is not a dry run.
    assert!(!vault.path().join(REPO_ID).exists());

    assert!(run_cli(&[]).status.success());
    assert!(vault.path().join(REPO_ID).is_dir());
}

#[test]
fn the_command_fails_when_a_run_could_not_be_verified() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path());
    std::fs::remove_file(run.join("artifacts/figure.png")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .arg("sync")
        .args(["--results-root", &results.path().to_string_lossy()])
        .args(["--repo-id", REPO_ID])
        .args(["--vault", &vault.path().to_string_lossy()])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("skip"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
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
    // Both spellings of --latest resolve the path the same way, so a shell can
    // compare them without knowing which branch answered.
    assert_eq!(
        PathBuf::from(&latest_run),
        std::fs::canonicalize(&child_dirs[1]).unwrap()
    );
    assert_eq!(
        PathBuf::from(&latest),
        std::fs::canonicalize(&parent_dir).unwrap()
    );
}

#[test]
fn a_sweep_child_is_not_a_run_started_by_hand() {
    // Both are `simulate`, so narrowing by subcommand alone hands back the last
    // child of the sweep rather than the run someone started themselves.
    let results = tempfile::tempdir().unwrap();
    let options = |sub: &str| {
        RunOptions::new("axelrod", sub)
            .repo_id("axelrod1997")
            .domain("simulation")
            .origin(Origin::Manual)
            .results_root(results.path())
            .parameters(&json!({"features": 5}))
            .unwrap()
            .master_seed(42)
    };

    let alone = Run::start(options("simulate")).unwrap();
    let alone_dir = alone.dir().to_path_buf();
    alone.finish().unwrap();

    let parent = Run::start(options("sweep").sweep_parent()).unwrap();
    let (sweep_id, parent_uid) = (
        parent.sweep_id().unwrap().to_string(),
        parent.run_uid().to_string(),
    );
    let mut children = Vec::new();
    for index in 0..2u64 {
        let child = Run::start(options("simulate").lineage(runvault::Lineage {
            sweep_id: Some(sweep_id.clone()),
            parent_run_uid: Some(parent_uid.clone()),
            ..Default::default()
        }))
        .unwrap();
        children.push(child.dir().to_path_buf());
        let _ = index;
        child.finish().unwrap();
    }
    parent.finish().unwrap();

    let runvault = |args: &[&str]| -> (bool, Vec<String>) {
        let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        (
            out.status.success(),
            stdout.lines().map(str::to_string).collect(),
        )
    };
    let root = results.path().to_string_lossy().to_string();
    let base = ["path", "--results-root", &root, "--experiment", "axelrod"];

    let (ok, lines) = runvault(&[&base[..], &["--latest", "--subcommand", "simulate"]].concat());
    assert!(ok);
    assert!(lines[0].contains(children[1].file_name().unwrap().to_str().unwrap()));

    let (ok, lines) = runvault(
        &[
            &base[..],
            &["--latest", "--subcommand", "simulate", "--standalone"],
        ]
        .concat(),
    );
    assert!(ok, "{lines:?}");
    assert_eq!(
        PathBuf::from(&lines[0]),
        std::fs::canonicalize(&alone_dir).unwrap()
    );

    let (ok, lines) = runvault(&[&base[..], &["--children-of", &parent_uid]].concat());
    assert!(ok);
    assert_eq!(lines.len(), 2, "{lines:?}");
    // Parent and children come back in the same shape, so they can be compared.
    for (line, dir) in lines.iter().zip(&children) {
        assert_eq!(PathBuf::from(line), std::fs::canonicalize(dir).unwrap());
    }
}
