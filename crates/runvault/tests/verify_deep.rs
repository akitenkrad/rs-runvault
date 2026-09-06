//! `runvault verify --deep`: the checks whose cost scales with the run.
//!
//! Every test here edits a *finished* run the way a person or a script could,
//! and asserts that the shallow checks still pass while the deep ones do not.
//! That pairing is the point: if the shallow checks caught it, the deep pass
//! would not be earning its cost.

use std::path::{Path, PathBuf};
use std::process::Command;

use runvault::{Run, RunOptions};
use serde_json::{Value, json};

/// A git repository with one commit, so `origin = code` records a lock file.
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

/// A finished run with an artifact, a lock file and a pair of trajectories.
///
/// Returned with the temporary directories so they outlive the run directory.
fn finished_run() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let repo = git_repo();
    let results = tempfile::tempdir().unwrap();
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id("social-simulation-replications")
            .domain("simulation")
            .results_root(results.path())
            .repo_root(repo.path())
            .parameters(&json!({"rows": 13, "cols": 16, "seed": 42}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(42),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();

    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts/segregation.csv"), "t,value\n0,0.31\n").unwrap();

    for (unit, t) in [("trial-0", 3.0), ("trial-1", 5.0)] {
        for step in 1..=(t as u64) {
            run.log_event(
                "observation",
                &json!({"unit_id": unit, "t": step as f64, "t_unit": "step"}),
            )
            .unwrap();
        }
        run.log_event(
            "terminal",
            &json!({"unit_id": unit, "t": t, "t_unit": "step",
                    "outcome": "converged", "censored": false, "budget": 100.0}),
        )
        .unwrap();
    }
    run.log_metric("segregation_index", 0.834)
        .step(120, "step")
        .send()
        .unwrap();
    run.finish().unwrap();

    (repo, results, dir)
}

/// A run that ended without `finish()`: an artifact and a log on disk, no
/// `manifest.csv`, and a `status.json` that records why it stopped.
///
/// This is the shape `runvault gc` leaves behind after it reaps a killed run.
/// It is reached here through `fail()`, which tears down in the same order and
/// writes no manifest either, so the test does not have to outrace a heartbeat.
fn unsealed_run(kind: &str) -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let repo = git_repo();
    let results = tempfile::tempdir().unwrap();
    let run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id("social-simulation-replications")
            .domain("simulation")
            .results_root(results.path())
            .repo_root(repo.path())
            .parameters(&json!({"rows": 13, "cols": 16, "seed": 42}))
            .unwrap()
            .seed_pointers(["/seed"])
            .master_seed(42),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();

    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts/grid.csv"), "t,value\n0,0.31\n").unwrap();
    std::fs::create_dir_all(dir.join("logs")).unwrap();
    std::fs::write(dir.join("logs/progress.log"), "step 1\n").unwrap();

    run.fail(kind, "the process stopped before it could seal the run")
        .unwrap();
    (repo, results, dir)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

/// The shallow checks pass, the deep ones do not, and the message names `field`.
#[track_caller]
fn only_deep_catches_it(dir: &Path, field: &str) {
    runvault::verify::shallow(dir).unwrap_or_else(|e| panic!("shallow は通るはずでした: {e}"));
    let err = runvault::verify::deep(dir).unwrap_err().to_string();
    assert!(err.contains(field), "{err}");
}

#[test]
fn a_finished_run_passes_both_depths() {
    let (_repo, _results, dir) = finished_run();
    runvault::verify::shallow(&dir).unwrap();
    runvault::verify::deep(&dir).unwrap();
}

#[test]
fn a_parameter_edited_after_the_run_no_longer_matches_its_condition() {
    let (_repo, _results, dir) = finished_run();
    let mut config = read_json(&dir.join("config.json"));
    config["parameters"]["rows"] = json!(14);
    write_json(&dir.join("config.json"), &config);

    // This is the whole reason the deep pass exists: the file still has the
    // right shape and the right `run_uid`, so nothing shallow objects, but the
    // run now claims a `config_hash` that describes a different condition.
    only_deep_catches_it(&dir, "config_hash");
}

#[test]
fn an_environment_edited_after_the_run_no_longer_matches_its_hash() {
    let (_repo, _results, dir) = finished_run();
    let mut meta = read_json(&dir.join("run.json"));
    meta["env"]["os"] = json!("plan9");
    write_json(&dir.join("run.json"), &meta);

    only_deep_catches_it(&dir, "env_hash");
}

#[test]
fn a_commit_edited_after_the_run_no_longer_matches_its_execution() {
    let (_repo, _results, dir) = finished_run();
    let mut meta = read_json(&dir.join("run.json"));
    meta["code"]["git_commit"] = json!("0".repeat(40));
    write_json(&dir.join("run.json"), &meta);

    // The condition and the environment are untouched, so only the third hash
    // moves: the same condition run against different code.
    only_deep_catches_it(&dir, "execution_hash");
}

#[test]
fn a_rewritten_lock_file_is_caught() {
    let (_repo, _results, dir) = finished_run();
    let lock = dir.join("lock/Cargo.lock");
    assert!(lock.is_file(), "the run recorded no lock file");
    std::fs::write(&lock, "# a different set of dependencies").unwrap();

    // `manifest.csv` covers `artifacts/` and `logs/` only, so without this
    // check the copy under `lock/` would be the one unguarded input to
    // `env_hash`.
    only_deep_catches_it(&dir, "Cargo.lock");
}

#[test]
fn an_artifact_rewritten_to_the_same_length_is_caught() {
    let (_repo, _results, dir) = finished_run();
    let artifact = dir.join("artifacts/segregation.csv");
    let before = std::fs::read(&artifact).unwrap();
    std::fs::write(&artifact, "t,value\n0,0.99\n").unwrap();
    assert_eq!(
        before.len(),
        std::fs::read(&artifact).unwrap().len(),
        "the byte count has to be unchanged, or the size check would catch it"
    );

    only_deep_catches_it(&dir, "ハッシュ");
}

#[test]
fn an_artifact_deleted_after_the_run_is_caught() {
    let (_repo, _results, dir) = finished_run();
    std::fs::remove_file(dir.join("artifacts/segregation.csv")).unwrap();

    only_deep_catches_it(&dir, "実在しません");
}

#[test]
fn a_generated_file_no_manifest_mentions_is_caught() {
    let (_repo, _results, dir) = finished_run();
    std::fs::write(dir.join("artifacts/extra.png"), "not in the record").unwrap();

    // Half a guarantee is the failure mode here: every recorded file would
    // still hash correctly while a figure exists that the record never names.
    only_deep_catches_it(&dir, "manifest.csv にありません");
}

#[test]
fn a_manifest_path_that_leaves_the_run_directory_is_refused() {
    let (_repo, _results, dir) = finished_run();
    let manifest = std::fs::read_to_string(dir.join("manifest.csv")).unwrap();
    std::fs::write(
        dir.join("manifest.csv"),
        manifest.replace("artifacts/segregation.csv", "../../../etc/hosts"),
    )
    .unwrap();

    let err = runvault::verify::deep(&dir).unwrap_err().to_string();
    assert!(err.contains("run ディレクトリの外"), "{err}");
}

/// Replaces `events.jsonl` with hand-written lines, as a second implementation
/// could produce: `log_event` refuses some of these, but a file written by
/// something other than this crate has never been through that gate.
fn rewrite_events(dir: &Path, lines: &[Value]) {
    let uid = read_json(&dir.join("run.json"))["run_uid"]
        .as_str()
        .unwrap()
        .to_string();
    let text: String = lines
        .iter()
        .map(|line| {
            let mut object = line.as_object().unwrap().clone();
            object.insert("run_uid".into(), json!(uid));
            object.insert("ts".into(), json!("2026-08-30T12:00:00+09:00"));
            format!("{}\n", serde_json::to_string(&object).unwrap())
        })
        .collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn observation(unit: &str, t: f64) -> Value {
    json!({"schema": "observation", "unit_id": unit, "t": t, "t_unit": "step"})
}

fn terminal(unit: &str, t: f64, censored: bool, budget: f64) -> Value {
    json!({"schema": "terminal", "unit_id": unit, "t": t, "t_unit": "step",
           "outcome": if censored { "unconverged" } else { "converged" },
           "censored": censored, "budget": budget})
}

#[test]
fn a_terminal_row_for_a_unit_that_was_never_observed_is_caught() {
    let (_repo, _results, dir) = finished_run();
    rewrite_events(
        &dir,
        &[
            observation("trial-0", 1.0),
            terminal("trial-9", 1.0, false, 100.0),
        ],
    );

    only_deep_catches_it(&dir, "observation に現れません");
}

#[test]
fn a_terminal_row_that_stops_short_of_its_own_observations_is_caught() {
    let (_repo, _results, dir) = finished_run();
    rewrite_events(
        &dir,
        &[
            observation("trial-0", 1.0),
            observation("trial-0", 7.0),
            terminal("trial-0", 3.0, false, 100.0),
        ],
    );

    // The summary row and the raw observations disagree about when the unit
    // stopped, which is exactly the case where a table built from the terminal
    // rows would not match one built from the trajectory.
    only_deep_catches_it(&dir, "最大 t");
}

#[test]
fn a_censored_row_whose_budget_was_never_reached_is_caught() {
    let (_repo, _results, dir) = finished_run();
    rewrite_events(
        &dir,
        &[
            observation("trial-0", 58.0),
            terminal("trial-0", 58.0, true, 100.0),
        ],
    );

    // `log_event` refuses this line, so it can only arrive from another
    // implementation — which is the case the deep pass has to cover.
    only_deep_catches_it(&dir, "budget");
}

#[test]
fn observations_may_arrive_out_of_order() {
    let (_repo, _results, dir) = finished_run();
    rewrite_events(
        &dir,
        &[
            observation("trial-0", 7.0),
            observation("trial-0", 2.0),
            terminal("trial-0", 7.0, false, 100.0),
        ],
    );

    // The rule is "the largest `t`", not "the last line": a run that writes
    // several units interleaved is not out of order.
    runvault::verify::deep(&dir).unwrap();
}

/// Rewrites the artifact's `manifest.csv` row with a different function and digest.
fn restate_manifest_digest(dir: &Path, algorithm: &str, digest: &str) {
    let text = std::fs::read_to_string(dir.join("manifest.csv")).unwrap();
    let rewritten: String = text
        .lines()
        .map(|line| {
            if !line.contains("artifacts/segregation.csv") {
                return format!("{line}\n");
            }
            let mut fields: Vec<&str> = line.split(',').collect();
            fields[2] = algorithm;
            fields[3] = digest;
            format!("{}\n", fields.join(","))
        })
        .collect();
    std::fs::write(dir.join("manifest.csv"), rewritten).unwrap();
}

#[test]
fn a_sha256_digest_is_checked_rather_than_passed_over() {
    // `schema/v1` accepts SHA-256 so a record can carry a digest that came from
    // elsewhere. The expected values below were produced by `shasum -a 256`,
    // not by this crate.
    let (_repo, _results, dir) = finished_run();
    restate_manifest_digest(
        &dir,
        "sha256",
        "44e88165374bcc4035fc4dba5cca7db206a45135ab0159b638b6d41c68463f1c",
    );
    runvault::verify::deep(&dir).unwrap();

    // Same length, different bytes: only the digest can tell, and skipping the
    // row for being SHA-256 would have called this run verified.
    std::fs::write(dir.join("artifacts/segregation.csv"), "t,value\n0,0.99\n").unwrap();
    only_deep_catches_it(&dir, "ハッシュ");

    restate_manifest_digest(
        &dir,
        "sha256",
        "0e533a991dabf0d878ca912bcdcf656ff3efb96074dd92adfedaecf5bf077473",
    );
    runvault::verify::deep(&dir).unwrap();
}

#[test]
fn a_digest_whose_function_cannot_be_named_is_not_agreement() {
    let (_repo, _results, dir) = finished_run();
    restate_manifest_digest(&dir, "md5", &"0".repeat(32));

    only_deep_catches_it(&dir, "照合できません");
}

// --- the manifest against a run that never sealed -----------------------------

#[test]
fn a_run_killed_before_it_sealed_is_not_asked_for_the_manifest_it_never_wrote() {
    let (_repo, _results, dir) = unsealed_run("killed");

    // `finish()` writes `manifest.csv`, so a killed run has none, and the files
    // it had already produced can never be listed in one. Refusing it here
    // refuses it forever: `runvault sync` would never carry the record of the
    // failure anywhere, and the run would sit in `results/` failing the daily
    // job until a person deleted the one thing that says it happened.
    runvault::verify::shallow(&dir).unwrap();
    runvault::verify::deep(&dir).unwrap();
}

#[test]
fn a_run_dropped_before_it_sealed_is_treated_the_same_way() {
    // Nothing about the exemption is specific to `gc`: what makes the invariant
    // unaskable is the missing seal, not which writer recorded the failure.
    let (_repo, _results, dir) = unsealed_run("dropped");
    runvault::verify::deep(&dir).unwrap();
}

#[test]
fn a_failed_run_that_did_seal_still_answers_for_a_file_that_appeared_later() {
    let (_repo, _results, dir) = finished_run();

    // Sealed first, recorded as failed second, and only then a new file beside
    // a manifest that does not name it. The exemption is the absent manifest,
    // never the verdict — otherwise editing `state` to `failed` would be enough
    // to walk any unrecorded artifact past the check.
    let mut status = read_json(&dir.join("status.json"));
    status["state"] = json!("failed");
    status["error"] = json!({"kind": "killed", "message": "not how this works"});
    write_json(&dir.join("status.json"), &status);
    std::fs::write(dir.join("artifacts/uninvited.csv"), "1\n").unwrap();

    only_deep_catches_it(&dir, "manifest.csv にありません");
}

#[test]
fn a_run_that_has_not_ended_at_all_still_answers_for_its_artifacts() {
    let (_repo, _results, dir) = finished_run();

    // No manifest and no status either: this is a run still in flight. Nothing
    // has said it failed, so nothing has excused it from the invariant.
    std::fs::remove_file(dir.join("manifest.csv")).unwrap();
    std::fs::remove_file(dir.join("status.json")).unwrap();

    only_deep_catches_it(&dir, "manifest.csv にありません");
}
