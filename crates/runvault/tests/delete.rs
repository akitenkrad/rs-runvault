use std::path::{Path, PathBuf};

use chrono::Local;
use runvault::delete::{self, DeleteStage, Presence, Selection};
use runvault::lockfile::LockRecord;
use runvault::meta::{Origin, Visibility};
use runvault::sync::{self, Planned, SyncOptions};
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

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

fn failed_run(results: &Path) -> (PathBuf, String, String) {
    let run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("other")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results),
    )
    .unwrap();
    let uid = run.run_uid().to_string();
    let slug = run.run_slug().to_string();
    let dir = run.fail("test", "expected failure").unwrap();
    (dir, uid, slug)
}

fn finished_run(results: &Path) -> (PathBuf, String) {
    let run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("other")
            .origin(Origin::Manual)
            .visibility(Visibility::Public)
            .results_root(results),
    )
    .unwrap();
    let uid = run.run_uid().to_string();
    (run.finish().unwrap(), uid)
}

fn sync_one(results: &Path, vault: &Path) {
    let options = SyncOptions {
        allow_internal: true,
        compress_over_bytes: u64::MAX,
    };
    for item in sync::plan_all(results, REPO_ID, vault, &options).unwrap() {
        if let Planned::Send(plan) = item {
            sync::execute(&plan).unwrap();
        }
    }
}

fn select(uid: &str) -> Selection {
    Selection::RunUids(vec![uid.to_string()])
}

fn tree(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    if !root.exists() {
        return paths;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            paths.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            }
        }
    }
    paths.sort();
    paths
}

#[test]
fn planning_is_a_true_dry_run_and_reports_both_locations_with_the_slug() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, uid, slug) = failed_run(results.path());
    sync_one(results.path(), vault.path());
    let before = (tree(results.path()), tree(vault.path()));

    let plans = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        select(&uid),
        "開発中の試行",
        false,
    )
    .unwrap();

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].presence(), Presence::Both);
    assert_eq!(plans[0].run_slug, slug);
    assert_eq!(before, (tree(results.path()), tree(vault.path())));
}

#[test]
fn repository_ids_cannot_escape_the_vault_root() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, uid, _) = failed_run(results.path());
    let error = delete::plan(
        results.path(),
        vault.path(),
        "../outside",
        select(&uid),
        "reason",
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("識別子"), "{error}");
    assert!(!vault.path().parent().unwrap().join("outside").exists());
}

#[test]
fn failed_is_allowed_but_finished_needs_include_succeeded() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, failed_uid, _) = failed_run(results.path());
    let (_, finished_uid) = finished_run(results.path());

    assert!(
        delete::plan(
            results.path(),
            vault.path(),
            REPO_ID,
            select(&failed_uid),
            "reason",
            false
        )
        .is_ok()
    );
    let error = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        select(&finished_uid),
        "reason",
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("--include-succeeded"), "{error}");
    assert!(
        delete::plan(
            results.path(),
            vault.path(),
            REPO_ID,
            select(&finished_uid),
            "reason",
            true
        )
        .is_ok()
    );
}

#[test]
fn live_and_stale_unfinished_runs_are_refused_for_different_reasons() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let live = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results.path()),
    )
    .unwrap();
    let live_uid = live.run_uid().to_string();
    let live_error = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        select(&live_uid),
        "reason",
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(live_error.contains("実行中"), "{live_error}");
    let stale_dir = live.dir().to_path_buf();
    std::mem::forget(live);

    let mut lock: LockRecord =
        runvault::files::read_json(&stale_dir.join(runvault::lockfile::LOCK_FILE)).unwrap();
    lock.pid = 0;
    lock.process_start_time = Some(1);
    lock.heartbeat_at = (Local::now() - chrono::Duration::hours(2)).to_rfc3339();
    runvault::lockfile::write(&stale_dir, &lock).unwrap();
    let stale_error = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        select(&live_uid),
        "reason",
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(stale_error.contains("runvault gc"), "{stale_error}");
}

#[test]
fn a_legacy_run_is_refused() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let dir = results.path().join("schelling/main_20240115_101500");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("status.json"),
        serde_json::to_vec(&json!({
            "schema_version": "1.0",
            "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X4Z",
            "state": "failed",
            "started_at": "2026-09-27T10:00:00+09:00",
            "finished_at": "2026-09-27T10:01:00+09:00",
            "duration_sec": 60.0,
            "error": {"kind": "test", "message": "failed"}
        }))
        .unwrap(),
    )
    .unwrap();
    let error = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        Selection::FailedExperiment("schelling".into()),
        "reason",
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("legacy"), "{error}");
}

#[test]
fn tombstone_then_vault_then_source_is_restartable_after_every_stage() {
    for stopped_after in [
        DeleteStage::Tombstone,
        DeleteStage::Vault,
        DeleteStage::Source,
    ] {
        let results = tempfile::tempdir().unwrap();
        let vault = private_vault();
        let (source, uid, _) = failed_run(results.path());
        sync_one(results.path(), vault.path());
        let plan = delete::plan(
            results.path(),
            vault.path(),
            REPO_ID,
            select(&uid),
            "reason",
            false,
        )
        .unwrap()
        .remove(0);
        let dest = plan.vault_dir.clone().unwrap();

        let error = delete::execute_with_hook(&plan, |stage| {
            if stage == stopped_after {
                Err(runvault::Error::Spec("injected stop".into()))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(error.to_string().contains("injected stop"));
        assert!(delete::contains(vault.path(), REPO_ID, &uid).unwrap());
        assert_eq!(dest.exists(), stopped_after == DeleteStage::Tombstone);
        assert_eq!(source.exists(), stopped_after != DeleteStage::Source);

        let retry = delete::plan(
            results.path(),
            vault.path(),
            REPO_ID,
            select(&uid),
            "reason",
            false,
        )
        .unwrap()
        .remove(0);
        if stopped_after == DeleteStage::Source {
            assert_eq!(retry.presence(), Presence::Neither);
        }
        delete::execute(&retry).unwrap();
        assert!(!dest.exists());
        assert!(!source.exists());
        let lines = std::fs::read_to_string(delete::tombstone_path(vault.path(), REPO_ID))
            .unwrap()
            .lines()
            .count();
        assert_eq!(lines, 1);
    }
}

#[test]
fn more_than_one_run_uid_can_be_selected_at_once() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, first, _) = failed_run(results.path());
    let (_, second, _) = failed_run(results.path());
    let plans = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        Selection::RunUids(vec![first.clone(), second.clone()]),
        "reason",
        false,
    )
    .unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(
        plans
            .into_iter()
            .map(|plan| plan.run_uid)
            .collect::<std::collections::BTreeSet<_>>(),
        [first, second].into_iter().collect()
    );
}

#[test]
fn a_shared_by_slug_directory_keeps_the_other_uid() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, uid, slug) = failed_run(results.path());
    sync_one(results.path(), vault.path());
    let slug_dir = vault
        .path()
        .join(REPO_ID)
        .join("schelling/by-slug")
        .join(&slug);
    let other = slug_dir.join("01K3QZ8F7H9M2N4P6R8T0V2X4Z");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        Path::new("../..").join("01K3QZ8F7H9M2N4P6R8T0V2X4Z"),
        &other,
    )
    .unwrap();

    let plan = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        select(&uid),
        "reason",
        false,
    )
    .unwrap()
    .remove(0);
    delete::execute(&plan).unwrap();

    assert!(slug_dir.is_dir());
    assert!(other.symlink_metadata().is_ok());
}

#[test]
fn source_only_and_vault_only_runs_can_each_be_deleted() {
    for keep_source in [true, false] {
        let results = tempfile::tempdir().unwrap();
        let vault = private_vault();
        let (source, uid, _) = failed_run(results.path());
        sync_one(results.path(), vault.path());
        let dest = vault.path().join(REPO_ID).join("schelling").join(&uid);
        if keep_source {
            std::fs::remove_dir_all(&dest).unwrap();
        } else {
            std::fs::remove_dir_all(&source).unwrap();
        }
        let plan = delete::plan(
            results.path(),
            vault.path(),
            REPO_ID,
            select(&uid),
            "reason",
            false,
        )
        .unwrap()
        .remove(0);
        assert_eq!(
            plan.presence(),
            if keep_source {
                Presence::SourceOnly
            } else {
                Presence::VaultOnly
            }
        );
        delete::execute(&plan).unwrap();
        assert!(!source.exists());
        assert!(!dest.exists());
    }
}

#[test]
fn failed_experiment_selection_deduplicates_the_two_copies() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, uid, _) = failed_run(results.path());
    finished_run(results.path());
    sync_one(results.path(), vault.path());
    let plans = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        Selection::FailedExperiment("schelling".into()),
        "reason",
        false,
    )
    .unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].run_uid, uid);
}

#[test]
fn tombstone_json_has_the_documented_shape() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let (_, uid, _) = failed_run(results.path());
    let plan = delete::plan(
        results.path(),
        vault.path(),
        REPO_ID,
        select(&uid),
        "開発中の試行",
        false,
    )
    .unwrap()
    .remove(0);
    delete::execute(&plan).unwrap();
    let line = std::fs::read_to_string(delete::tombstone_path(vault.path(), REPO_ID)).unwrap();
    let value: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(value["schema_version"], "1.0");
    assert_eq!(value["run_uid"], uid);
    assert_eq!(value["reason"], "開発中の試行");
    assert_eq!(value["state"], "failed");
}
