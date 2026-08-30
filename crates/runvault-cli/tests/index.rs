//! `runvault query`: the aggregation repository, flattened and asked questions.
//!
//! The queries exercised here are the ones §5.2 of the design note publishes,
//! run verbatim, so the documented SQL and the writer cannot drift apart.

use std::path::{Path, PathBuf};
use std::process::Command;

use runvault::meta::{Dataset, Origin, Target, Visibility, Work};
use runvault::sync::{self, Planned, SyncOptions};
use runvault::{Run, RunOptions};
use serde_json::json;

const REPO_ID: &str = "social-simulation-replications";

fn private_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(sync::VAULT_CONFIG),
        "schema_version = \"1.0\"\nvisibility = \"private\"\n",
    )
    .unwrap();
    dir
}

/// A replication run: a paper, a target, a reported value and a reproduced one.
fn replication_run(results: &Path, seed: u64, reproduced: f64) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results)
            .parameters(&json!({"rows": 13, "cols": 16, "seed": seed}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(seed)
            .data([Dataset::init("grid")
                .dataset_id("schelling-grid-v1")
                .version("v1")
                .n(208)])
            .replication(
                Work::doi("10.1080/0022250X.1971.9989794")
                    .title("Dynamic Models of Segregation")
                    .year(1971)
                    .source_version("published")
                    .target(Target::table("tbl3-r2", "Table 3").row("2"))
                    .obsidian_note("研究/98_論文レポート/80-再現実験/P00000009/設計書.md")
                    .jira("MYTASK-3057"),
            ),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.log_metric("segregation_index", reproduced)
        .send()
        .unwrap();
    run.log_metric("segregation_index", reproduced - 0.1)
        .step(10, "step")
        .send()
        .unwrap();
    run.log_reference("segregation_index", 0.90)
        .target("tbl3-r2")
        .source("paper")
        .send()
        .unwrap();
    run.finish().unwrap();
    dir
}

/// A run directory in the shape the repositories used before this specification.
fn legacy_run(results: &Path) -> PathBuf {
    let dir = results.join("axelrod").join("simulate_20240115_101500");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"features": 5}"#).unwrap();
    std::fs::write(dir.join("metrics.csv"), "t,regions\n0,120\n10,44\n20,7\n").unwrap();
    dir
}

/// Syncs everything under `results` into `vault`.
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

/// Runs `runvault query` in the vault and returns `(success, stdout)`.
fn query(vault: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_runvault"))
        .arg("query")
        .args(["--vault", &vault.to_string_lossy()])
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

#[test]
fn the_index_holds_both_kinds_of_run_under_one_key() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let canonical = replication_run(results.path(), 42, 0.83);
    legacy_run(results.path());
    sync_all(results.path(), vault.path());

    let (ok, stdout) = query(vault.path(), &["--refresh"]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("runs\t2 行"), "{stdout}");
    for table in [
        "runs",
        "metrics",
        "reference",
        "manifest",
        "run_data",
        "run_targets",
        "run_jira",
    ] {
        assert!(
            vault
                .path()
                .join("index")
                .join(format!("{table}.parquet"))
                .is_file(),
            "{table}.parquet がありません"
        );
    }

    let uid = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string(canonical.join("run.json")).unwrap(),
    )
    .unwrap()["run_uid"]
        .as_str()
        .unwrap()
        .to_string();

    let (ok, stdout) = query(
        vault.path(),
        &["SELECT run_key, run_uid, experiment, state FROM 'index/runs.parquet' ORDER BY run_key"],
    );
    assert!(ok, "{stdout}");
    // A legacy run keeps the key it had at the source and carries no `run_uid`;
    // the canonical one is keyed by its `run_uid`.
    assert!(
        stdout.contains(&format!(
            "legacy:{REPO_ID}:axelrod/simulate_20240115_101500\tNULL\taxelrod\tNULL"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{uid}\t{uid}\tschelling\tfinished")),
        "{stdout}"
    );
}

#[test]
fn a_legacy_run_contributes_its_metrics_without_a_run_uid() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    legacy_run(results.path());
    sync_all(results.path(), vault.path());
    assert!(query(vault.path(), &["--refresh"]).0);

    let (ok, stdout) = query(
        vault.path(),
        &["SELECT step, step_unit, scope, name, value FROM 'index/metrics.parquet' ORDER BY step"],
    );
    assert!(ok, "{stdout}");
    assert_eq!(stdout.lines().count(), 4, "{stdout}");
    assert!(stdout.contains("regions"), "{stdout}");
}

#[test]
fn only_the_runs_a_paper_may_cite_are_returned() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    legacy_run(results.path());
    sync_all(results.path(), vault.path());
    assert!(query(vault.path(), &["--refresh"]).0);

    // §5.2, first query, verbatim.
    let (ok, stdout) = query(
        vault.path(),
        &["SELECT r.experiment, r.config_hash, avg(m.value) AS asr \
           FROM 'index/runs.parquet'    AS r \
           JOIN 'index/metrics.parquet' AS m USING (run_key) \
           WHERE r.git_dirty = false AND r.state = 'finished' AND m.name = 'asr' \
           GROUP BY 1, 2 ORDER BY asr DESC;"],
    );
    assert!(ok, "{stdout}");

    // The same query for a metric that exists finds the run; the legacy one is
    // excluded by `state = 'finished'`, which it cannot claim.
    let (ok, stdout) = query(
        vault.path(),
        &["SELECT r.experiment, count(*) AS n \
           FROM 'index/runs.parquet'    AS r \
           JOIN 'index/metrics.parquet' AS m USING (run_key) \
           WHERE r.state = 'finished' AND m.name = 'segregation_index' GROUP BY 1;"],
    );
    assert!(ok, "{stdout}");
    assert!(stdout.contains("schelling\t2"), "{stdout}");
}

#[test]
fn the_same_condition_running_in_two_environments_is_visible() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    sync_all(results.path(), vault.path());
    assert!(query(vault.path(), &["--refresh"]).0);

    // §5.2, second query, verbatim. One machine, so nothing is reported — the
    // point is that the columns it names exist and the SQL runs.
    let (ok, stdout) = query(
        vault.path(),
        &[
            "SELECT r.config_hash, count(DISTINCT r.env_hash) AS n_env, count(*) AS n_run \
           FROM 'index/runs.parquet' AS r \
           WHERE r.state = 'finished' \
           GROUP BY 1 HAVING n_env > 1;",
        ],
    );
    assert!(ok, "{stdout}");
    assert!(stdout.trim().is_empty(), "{stdout}");
}

#[test]
fn the_difference_from_the_reported_value_joins_on_nulls_as_equal() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    sync_all(results.path(), vault.path());
    assert!(query(vault.path(), &["--refresh"]).0);

    // §5.2, third query, verbatim. The reported value has no `step`, and so
    // does the aggregate it is compared with: `IS NOT DISTINCT FROM` is what
    // makes those two NULLs meet. Plain `=` would return nothing at all.
    let (ok, stdout) = query(
        vault.path(),
        &[
            "SELECT m.run_key, m.name, f.target_id, m.value AS reproduced, f.value AS reported, \
                  m.value - f.value AS diff \
           FROM 'index/metrics.parquet'   AS m \
           JOIN 'index/reference.parquet' AS f \
             ON  m.run_key = f.run_key \
             AND m.name    = f.name \
             AND m.scope   = f.scope \
             AND m.step      IS NOT DISTINCT FROM f.step \
             AND m.step_unit IS NOT DISTINCT FROM f.step_unit;",
        ],
    );
    assert!(ok, "{stdout}");
    // Exactly one row: the run-scope aggregate, not the step-10 series point.
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    assert!(stdout.contains("tbl3-r2"), "{stdout}");

    let (ok, plain) = query(
        vault.path(),
        &["SELECT count(*) AS n FROM 'index/metrics.parquet' AS m \
           JOIN 'index/reference.parquet' AS f \
             ON m.run_key = f.run_key AND m.name = f.name AND m.step = f.step;"],
    );
    assert!(ok, "{plain}");
    assert!(
        plain.contains("\n0"),
        "= で結合すると 0 行になるはずでした: {plain}"
    );
}

#[test]
fn the_paper_and_the_issue_travel_with_the_run() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    sync_all(results.path(), vault.path());
    assert!(query(vault.path(), &["--refresh"]).0);

    let (ok, stdout) = query(
        vault.path(),
        &[
            "SELECT r.title, r.year, t.label, t.row, j.issue_key, d.role, d.n \
           FROM 'index/runs.parquet'        AS r \
           JOIN 'index/run_targets.parquet' AS t USING (run_key) \
           JOIN 'index/run_jira.parquet'    AS j USING (run_key) \
           JOIN 'index/run_data.parquet'    AS d USING (run_key);",
        ],
    );
    assert!(ok, "{stdout}");
    assert!(stdout.contains("Dynamic Models of Segregation"), "{stdout}");
    assert!(stdout.contains("MYTASK-3057"), "{stdout}");
    assert!(
        stdout.contains("tbl3-r2") || stdout.contains("Table 3"),
        "{stdout}"
    );
}

#[test]
fn refreshing_twice_does_not_double_the_rows() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    replication_run(results.path(), 42, 0.83);
    sync_all(results.path(), vault.path());

    let (ok, first) = query(vault.path(), &["--refresh"]);
    assert!(ok, "{first}");
    let (ok, second) = query(vault.path(), &["--refresh"]);
    assert!(ok, "{second}");
    // The index is rebuilt, not amended: it describes the repository as it is,
    // and an amended table would keep describing runs that are gone.
    assert_eq!(first, second);
}

#[test]
fn asking_for_neither_a_refresh_nor_a_query_is_an_error() {
    let vault = private_vault();
    assert!(!query(vault.path(), &[]).0);
}
