//! `runvault metrics audit`: how much of the vault says what it measured.
//!
//! Descriptions became required at the point a metric is recorded (2026-09-08),
//! but the 1,222 runs already in the vault predate the rule and cannot be given
//! descriptions after the fact. So the audit still has undescribed names to
//! count, and these fixtures reproduce them the only way a run can carry one
//! now: the repository has said `require_docs = false`. These tests hold the
//! audit to the three things it has to get right: the split per experiment, the
//! folding of a generated tree into the family that describes it, and saying so
//! when the list of undescribed names is cut.

use std::path::{Path, PathBuf};
use std::process::Command;

use runvault::meta::{Origin, Visibility};
use runvault::sync::{self, Planned, SyncOptions};
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

const REPO_ID: &str = "social-simulation-replications";

/// The declaration javitz1991 would carry: one name, one family.
///
/// `require_docs = false` because these runs stand in for the ones recorded
/// before descriptions were required — the names the audit exists to count
/// could not be recorded at all under the rule.
const DECLARATION: &str = r#"
require_docs = false

[metrics."convergence_rate"]
meaning = "収束した試行の割合"
unit = "ratio"
direction = "up"
range = [0.0, 1.0]

[[metric_patterns]]
pattern = "{scenario}.q_ordinal.{index}.{agg}"
meaning = "IDES の順序統計量"
[metric_patterns.axes]
scenario = "投入した通信シナリオ"
index = "成分番号"
agg = "その成分の集約"
"#;

fn private_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(sync::VAULT_CONFIG),
        "schema_version = \"1.0\"\nvisibility = \"private\"\n",
    )
    .unwrap();
    dir
}

/// A repository, with the declaration it carries.
///
/// `None` is a repository that describes nothing — written as the one line that
/// says so, rather than as a missing file, because a missing file now refuses
/// the first metric outright.
fn repo_with(declaration: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let text = declaration.unwrap_or("require_docs = false\n");
    std::fs::write(dir.path().join("runvault.toml"), text).unwrap();
    dir
}

/// A run of `experiment` that records `metrics`, under `repo`'s declaration.
fn run_with(repo: &Path, results: &Path, experiment: &str, metrics: &[&str]) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new(experiment, "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .repo_root(repo)
            .parameters(&json!({"rows": 13, "seed": 42}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(42),
    )
    .unwrap();
    for (i, name) in metrics.iter().enumerate() {
        run.log_metric(*name, i as f64).send().unwrap();
    }
    run.finish().unwrap()
}

/// The eleven names one javitz-shaped run records.
///
/// Six of them belong to the declared family, one is declared by name, one is
/// reserved by the core vocabulary and three are nobody's.
fn javitz_metrics() -> Vec<&'static str> {
    vec![
        "convergence_rate",
        "n_units",
        "normal.q_ordinal.0.mean",
        "normal.q_ordinal.1.mean",
        "normal.q_ordinal.0.max",
        "masquerade.q_ordinal.0.mean",
        "masquerade.q_ordinal.1.mean",
        "masquerade.q_ordinal.1.max",
        "mean_n_surviving",
        "q_ordinal.total",
        "warmup_steps",
    ]
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

fn runvault(vault: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .args(args)
        .args(["--vault", &vault.to_string_lossy()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Syncs, indexes, and returns the audit as both a report and its JSON.
fn audit(results: &Path, vault: &Path, extra: &[&str]) -> (String, Value) {
    sync_all(results, vault);
    runvault(vault, &["query", "--refresh"]);
    let mut args = vec!["metrics", "audit"];
    args.extend_from_slice(extra);
    let text = runvault(vault, &args);
    let mut json_args = args.clone();
    json_args.push("--json");
    let json: Value = serde_json::from_str(&runvault(vault, &json_args)).unwrap();
    (text, json)
}

fn experiment<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["experiment"] == json!(name))
        .unwrap_or_else(|| panic!("{name} が数えられていません: {report}"))
}

#[test]
fn every_experiment_is_split_into_described_and_not() {
    let repo = repo_with(Some(DECLARATION));
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    run_with(repo.path(), results.path(), "javitz", &javitz_metrics());
    let (text, report) = audit(results.path(), vault.path(), &[]);

    let counted = experiment(&report, "javitz");
    assert_eq!(counted["n_names"], json!(11), "{text}");
    // 宣言した名前 1・予約語 1・形に当たった 6．
    assert_eq!(counted["n_described"], json!(8), "{text}");
    assert_eq!(counted["n_by_name"], json!(1), "{text}");
    assert_eq!(counted["n_by_vocabulary"], json!(1), "{text}");
    assert_eq!(counted["n_undescribed"], json!(3), "{text}");
    assert!(
        text.contains("指標名 11 種類 — 説明あり 8 ／ 説明なし 3"),
        "{text}"
    );
}

#[test]
fn a_generated_tree_is_folded_into_the_family_that_describes_it() {
    // 11,309 件をそのまま並べても読めない．形に当たったものは 1 行にまとめる．
    let repo = repo_with(Some(DECLARATION));
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    run_with(repo.path(), results.path(), "javitz", &javitz_metrics());
    let (text, report) = audit(results.path(), vault.path(), &[]);

    let families = experiment(&report, "javitz")["families"]
        .as_array()
        .unwrap();
    assert_eq!(families.len(), 1, "{text}");
    assert_eq!(
        families[0]["pattern"],
        json!("{scenario}.q_ordinal.{index}.{agg}")
    );
    assert_eq!(families[0]["n_matched"], json!(6), "{text}");
    assert_eq!(families[0]["meaning"], json!("IDES の順序統計量"));
    assert!(
        text.contains("形の宣言 {scenario}.q_ordinal.{index}.{agg}  6 件 — IDES の順序統計量"),
        "{text}"
    );
    // 区画の数が合わない名前は，形が似ていても当たらない．
    let undescribed = experiment(&report, "javitz")["undescribed"]
        .as_array()
        .unwrap();
    assert!(
        undescribed.contains(&json!("q_ordinal.total")),
        "{undescribed:?}"
    );
}

#[test]
fn a_cut_list_says_that_it_was_cut() {
    let repo = repo_with(Some(DECLARATION));
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    run_with(repo.path(), results.path(), "javitz", &javitz_metrics());
    let (text, report) = audit(results.path(), vault.path(), &["--limit", "2"]);

    assert!(
        text.contains("説明なし 3 件のうち 2 件を出しています"),
        "{text}"
    );
    let counted = experiment(&report, "javitz");
    // 打ち切っても «いくつあるか» は変わらない．
    assert_eq!(counted["n_undescribed"], json!(3));
    assert_eq!(counted["n_undescribed_shown"], json!(2));
    assert_eq!(counted["undescribed"].as_array().unwrap().len(), 2);
}

#[test]
fn a_repository_with_no_declaration_has_nothing_described() {
    // 既存の 5 リポジトリの姿．監査はこれを数えるために足した．
    let repo = repo_with(None);
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    run_with(
        repo.path(),
        results.path(),
        "schelling",
        &["segregation_index", "mean_n_surviving"],
    );
    let (text, report) = audit(results.path(), vault.path(), &[]);

    let counted = experiment(&report, "schelling");
    assert_eq!(counted["n_names"], json!(2), "{text}");
    assert_eq!(counted["n_described"], json!(0), "{text}");
    assert_eq!(
        counted["undescribed"],
        json!(["mean_n_surviving", "segregation_index"]),
        "{text}"
    );
}

#[test]
fn the_experiments_are_counted_apart_from_one_another() {
    let described = repo_with(Some(DECLARATION));
    let plain = repo_with(None);
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    run_with(
        described.path(),
        results.path(),
        "javitz",
        &["convergence_rate", "mean_n_surviving"],
    );
    run_with(
        plain.path(),
        results.path(),
        "schelling",
        &["convergence_rate", "segregation_index"],
    );
    let (text, report) = audit(results.path(), vault.path(), &[]);

    // 同じ `convergence_rate` でも，宣言のある実験だけが説明を持つ．
    assert_eq!(
        experiment(&report, "javitz")["n_described"],
        json!(1),
        "{text}"
    );
    assert_eq!(
        experiment(&report, "schelling")["n_described"],
        json!(0),
        "{text}"
    );
    assert_eq!(report["totals"]["n_experiments"], json!(2), "{text}");
    assert_eq!(report["totals"]["n_described"], json!(1), "{text}");
    assert_eq!(report["totals"]["n_undescribed"], json!(3), "{text}");
}

#[test]
fn the_audit_asks_for_the_index_rather_than_guessing_without_it() {
    let vault = private_vault();
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .args(["metrics", "audit"])
        .args(["--vault", &vault.path().to_string_lossy()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let message = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(message.contains("runvault query --refresh"), "{message}");
}

/// The audit is not a gate: `verify` keeps passing on a run nothing describes.
#[test]
fn an_undescribed_run_is_still_a_valid_run() {
    let repo = repo_with(None);
    let results = tempfile::tempdir().unwrap();
    let run_dir = run_with(repo.path(), results.path(), "schelling", &["undocumented"]);
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .args(["verify".as_ref(), run_dir.as_os_str()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
