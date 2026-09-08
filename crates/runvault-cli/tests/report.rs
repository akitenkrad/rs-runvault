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
                    .target(
                        Target::table("tbl3-r2", "Table 3")
                            .row("2")
                            .condition("30% preference, 2000 agents"),
                    )
                    // Two targets of the same figure. The label is the same for
                    // both, so a report that carries only the label shows them
                    // as one target recorded twice.
                    .target(Target::figure("fig1-a", "Figure 1").panel("a"))
                    .target(Target::figure("fig1-b", "Figure 1").panel("b"))
                    // Two claims of the same section. Neither has a panel or a
                    // row, so the condition is the only thing that tells them
                    // apart.
                    .target(
                        Target::claim("say-1", "section 2.2")
                            .condition("segregation is not intended"),
                    )
                    .target(
                        Target::claim("say-2", "section 2.2")
                            .condition("a mild preference suffices"),
                    )
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
    assert_eq!(report["schema_version"], json!("1.6"));
    assert!(report["vocab_version"].as_str().is_some());
    assert_eq!(report["freshness_hours"], json!(24.0));
    // The list is not capped any more, so there is no rule to report.
    assert!(report.get("run_limit").is_none(), "{report:?}");
    // What a run recorded, next to what the summary is carrying.
    assert!(report["runs"][0]["n_metrics"].as_i64().unwrap() >= 0);
}

#[test]
fn a_run_carries_the_sweep_it_belongs_to_and_the_hashes_it_is_compared_by() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let runs = report["runs"].as_array().unwrap();
    let run = &runs[0];
    // Both hashes are what the dashboard puts side by side: same condition with
    // a different environment is the `env_split` warning, and reading them from
    // `run.json` meant opening every run to answer it.
    assert!(
        run["config_hash"].as_str().is_some_and(|v| v.len() == 64),
        "{run:?}"
    );
    assert!(
        run["env_hash"].as_str().is_some_and(|v| v.len() == 64),
        "{run:?}"
    );
    // A run outside a sweep records neither, and says so rather than guessing.
    assert!(run.get("sweep_id").is_some(), "{run:?}");
    assert!(run.get("parent_run_uid").is_some(), "{run:?}");
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

    // The experiment carries the issue key of its runs. `runs[]` is capped, so
    // a dashboard grouping by research theme from `runs[]` alone loses whole
    // repositories once their runs fall outside the newest N — they read as
    // "nothing was ever run here" rather than as a smaller count.
    assert_eq!(experiments[0]["jira"], json!(["MYTASK-3058"]));
}

/// An experiment whose runs never recorded an issue key.
///
/// The empty array is the answer: no key was written. Filling it from the
/// repository name or from a sibling experiment would invent the one thing the
/// grouping trusts.
#[test]
fn an_experiment_without_an_issue_key_says_so_with_an_empty_list() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    legacy_run_at_the_root(results.path(), "20240101_000000");
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let experiments = report["experiments"].as_array().unwrap();
    assert_eq!(experiments[0]["jira"], json!([]));
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
    // Targets that share a label stay apart, and the field name keeps a bare
    // `2` readable. Ordered by `target_id`, so the report does not shuffle.
    assert_eq!(
        run["replication"]["targets"],
        json!([
            "Figure 1 panel a",
            "Figure 1 panel b",
            "section 2.2 — segregation is not intended",
            "section 2.2 — a mild preference suffices",
            // The row already tells this one apart, so its condition stays out.
            "Table 3 row 2"
        ])
    );
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

/// A repository that declares what its metrics mean, and a run under it.
///
/// The declaration is the source (§3.11); everything the dashboard shows about
/// a metric has to have come from here.
fn declared_run(results: &Path, experiment: &str, metrics: &[&str]) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("runvault.toml"),
        r#"
        [metrics."segregation_index"]
        meaning = "同類に囲まれている度合い"
        unit = "ratio"
        direction = "down"
        range = [0.0, 1.0]

        [metrics."final_iteration"]
        meaning = "止まるまでの反復回数"
        unit = "count"

        [parameters."/rows"]
        meaning = "格子の行数"
        unit = "count"

        [parameters."/seed"]
        meaning = "この試行の乱数種"

        [[metric_patterns]]
        pattern = "{scenario}.q_ordinal.{index}.{agg}"
        meaning = "IDES の順序統計量"
        unit = "count"
        [metric_patterns.axes]
        scenario = "投入した通信シナリオ"
        index = "成分番号"
        agg = "その成分の集約"
        "#,
    )
    .unwrap();
    let mut run = Run::start(
        RunOptions::new(experiment, "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .repo_root(repo.path())
            .parameters(&json!({"rows": 13, "seed": 7}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(7),
    )
    .unwrap();
    for (i, name) in metrics.iter().enumerate() {
        run.log_metric(*name, i as f64).send().unwrap();
    }
    run.finish().unwrap();
    repo
}

fn experiment_named<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["experiment"] == json!(name))
        .unwrap_or_else(|| panic!("{name} がありません: {report}"))
}

#[test]
fn an_experiment_carries_what_its_metrics_mean() {
    // 説明は run ではなく実験の性質なので `experiments[]` に載る．`runs[].metrics`
    // は主要な 12 件しか載せていないので，そちらに付けると届かない．
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let _repo = declared_run(
        results.path(),
        "schelling",
        &[
            "segregation_index",
            "final_iteration",
            "normal.q_ordinal.0.mean",
            "undocumented_metric",
        ],
    );
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let docs = &experiment_named(&report, "schelling")["metric_docs"];
    assert_eq!(
        docs["names"]["segregation_index"],
        json!({"meaning": "同類に囲まれている度合い", "unit": "ratio", "direction": "down"})
    );
    // `direction` を書かなかった指標には，画面が使える «どちらが良いか» が無い．
    assert_eq!(docs["names"]["final_iteration"]["direction"], json!(null));
    assert_eq!(docs["patterns"].as_array().unwrap().len(), 1);
    assert_eq!(
        docs["patterns"][0]["pattern"],
        json!("{scenario}.q_ordinal.{index}.{agg}")
    );
    assert_eq!(docs["patterns"][0]["axes"]["index"], json!("成分番号"));
    // 誰も説明していない名前は，説明が無いまま残る．埋めない．
    assert!(docs["names"].get("undocumented_metric").is_none(), "{docs}");
}

#[test]
fn the_reserved_names_are_explained_once_rather_than_per_experiment() {
    // `n_units` はほぼ全ての run にある．実験ごとに写すと，語彙が決めた意味を
    // 実験が宣言したように見せてしまう．
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let _repo = declared_run(results.path(), "schelling", &["segregation_index"]);
    let report = payload(results.path(), vault.path());

    assert!(
        report["metric_vocabulary"]["n_units"]["meaning"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{report}"
    );
    let docs = &experiment_named(&report, "schelling")["metric_docs"];
    assert!(docs["names"].get("n_units").is_none(), "{docs}");
}

#[test]
fn an_experiment_that_declared_nothing_says_so_with_an_empty_pair() {
    // 既存の 5 リポジトリの姿．鍵が無いのと «空» を分けておかないと，画面が
    // «読み込み中» と «説明が無い» を区別できない．
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let docs = &experiment_named(&report, "schelling")["metric_docs"];
    assert_eq!(docs["names"], json!({}), "{docs}");
    assert_eq!(docs["patterns"], json!([]), "{docs}");
}

/// A sweep of two points, each named and numbered (MYTASK-3232).
fn named_sweep(results: &Path) {
    let parent = Run::start(
        RunOptions::new("schelling", "sweep")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .parameters(&json!({"grid": "threshold"}))
            .unwrap()
            .label("τ を 2 点振る")
            .sweep_parent(),
    )
    .unwrap();
    let sweep_id = parent.sweep_id().unwrap().to_string();
    let parent_uid = parent.meta().run_uid.clone();
    parent.finish().unwrap();

    for (i, tau) in [0.33, 0.5].iter().enumerate() {
        let mut child = Run::start(
            RunOptions::new("schelling", "sweep-point")
                .repo_id(REPO_ID)
                .domain("simulation")
                .origin(Origin::Manual)
                .visibility(Visibility::Public)
                .results_root(results)
                .parameters(&json!({"threshold": tau, "seed": 1}))
                .unwrap()
                .seed_pointers(["/seed"])
                .master_seed(1)
                .label(format!("τ={tau}"))
                .sweep_point(i as u64, 2)
                .lineage(runvault::Lineage {
                    sweep_id: Some(sweep_id.clone()),
                    parent_run_uid: Some(parent_uid.clone()),
                    ..Default::default()
                }),
        )
        .unwrap();
        child
            .log_metric("segregation_index", 0.7 + i as f64)
            .send()
            .unwrap();
        child.finish().unwrap();
    }
}

#[test]
fn a_run_carries_the_name_it_was_given_and_its_place_in_the_sweep() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    named_sweep(results.path());
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let runs = report["runs"].as_array().unwrap();
    let point = runs
        .iter()
        .find(|r| r["label"] == json!("τ=0.5"))
        .unwrap_or_else(|| panic!("名前で引けません: {report}"));
    assert_eq!(point["sweep_index"], json!(1));
    assert_eq!(point["sweep_total"], json!(2));
    // 画面は «2/2» と出す．記録は 0 始まりで，+1 は読ませる側の仕事．
    assert!(point["sweep_id"].as_str().is_some());

    let parent = runs
        .iter()
        .find(|r| r["subcommand"] == json!("sweep"))
        .expect("親");
    assert_eq!(parent["label"], json!("τ を 2 点振る"));
    // 親はグリッドの点ではないので番号を持たない．
    assert_eq!(parent["sweep_index"], json!(null));
}

#[test]
fn a_run_with_no_name_carries_null_rather_than_an_empty_one() {
    // 既存の 1,222 run はどれも名前を持たない．画面はこれを «読み込み中» と
    // 取り違えてはいけないので，鍵はあって値が null という形にする．
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let run = &report["runs"][0];
    assert!(run.as_object().unwrap().contains_key("label"), "{run}");
    assert_eq!(run["label"], json!(null));
    assert_eq!(run["sweep_index"], json!(null));
    assert_eq!(run["sweep_total"], json!(null));
}

#[test]
fn an_experiment_carries_what_its_conditions_are() {
    // 条件も指標と同じで «実験の性質» なので `experiments[]` に載る．
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let _repo = declared_run(results.path(), "schelling", &["segregation_index"]);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    let docs = &experiment_named(&report, "schelling")["parameter_docs"];
    assert_eq!(docs["/rows"]["meaning"], json!("格子の行数"));
    assert_eq!(docs["/rows"]["unit"], json!("count"));
    assert_eq!(docs["/seed"]["unit"], json!(null));
    // 宣言していない設定は載らない（画面が «説明が記録されていません» と出す）．
    assert!(docs.get("/threshold").is_none(), "{docs}");
}

#[test]
fn an_experiment_that_described_no_condition_carries_an_empty_object() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    let report = payload(results.path(), vault.path());
    assert_valid(&report);

    // 鍵はあって空．«まだ読めていない» と区別できる形にしておく．
    assert_eq!(
        experiment_named(&report, "schelling")["parameter_docs"],
        json!({})
    );
}
