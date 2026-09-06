//! `runvault sync`: what leaves the machine, what is held back, and what a
//! second sync of the same run is allowed to change.
//!
//! Two things are being protected here. The record must survive the loss of one
//! laptop, and a prompt or a capture must not enter a git history that cannot
//! forget it. Almost every test below is about the second.

use std::path::{Path, PathBuf};

use jsonschema::{Retrieve, Uri, Validator};
use runvault::meta::{Origin, Visibility};
use runvault::sync::{self, Compression, Planned, SyncOptions};
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

// --- schema plumbing, so the receipt is checked against schema/v1 ------------

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

fn validator(name: &str) -> Validator {
    let text = std::fs::read_to_string(schema_dir().join(format!("{name}.json"))).unwrap();
    jsonschema::options()
        .should_validate_formats(true)
        .with_retriever(SchemaFiles)
        .build(&serde_json::from_str(&text).unwrap())
        .expect("schema compiles")
}

#[track_caller]
fn assert_valid(name: &str, instance: &Value) {
    let errors: Vec<String> = validator(name)
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

// --- fixtures ----------------------------------------------------------------

const REPO_ID: &str = "social-simulation-replications";

/// An aggregation repository that declares itself private.
fn vault(declaration: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(sync::VAULT_CONFIG), declaration).unwrap();
    dir
}

fn private_vault() -> tempfile::TempDir {
    vault("schema_version = \"1.0\"\nvisibility = \"private\"\n")
}

fn options(allow_internal: bool) -> SyncOptions {
    SyncOptions {
        allow_internal,
        compress_over_bytes: 10 * 1024 * 1024,
    }
}

/// A finished run, public unless told otherwise, with an artifact and events.
fn finished_run(results: &Path, visibility: Visibility) -> PathBuf {
    let mut run = Run::start(
        RunOptions::new("schelling", "main")
            .repo_id(REPO_ID)
            .domain("simulation")
            .origin(Origin::Manual)
            .visibility(visibility)
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
    run.log_event(
        "observation",
        &json!({"unit_id": "u0", "t": 1.0, "t_unit": "step"}),
    )
    .unwrap();
    run.log_metric("segregation_index", 0.83).send().unwrap();
    run.finish().unwrap();
    dir
}

/// A run whose process was killed and which `runvault gc` then recorded as a
/// failure: artifacts on disk, and no `manifest.csv`, because `finish()` never
/// ran to write one.
fn killed_run(results: &Path) -> PathBuf {
    let run = Run::start(
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
    std::fs::write(dir.join("artifacts/grid.csv"), vec![0u8; 512]).unwrap();
    run.fail("killed", "pid 16007 が status.json を書かずに終了しました")
        .unwrap();
    dir
}

fn plan_one(results: &Path, run: &Path, vault: &Path, allow_internal: bool) -> Planned {
    sync::plan_canonical(run, REPO_ID, vault, &options(allow_internal))
        .unwrap_or_else(|e| panic!("{}: {e}", results.display()))
}

#[track_caller]
fn sent(planned: Planned) -> Box<runvault::sync::SyncPlan> {
    match planned {
        Planned::Send(plan) => plan,
        Planned::Skipped { reason, .. } => panic!("送られませんでした: {reason}"),
    }
}

// --- the declaration ---------------------------------------------------------

#[test]
fn a_destination_that_never_said_it_was_private_is_refused() {
    let empty = tempfile::tempdir().unwrap();
    let err = sync::load_vault_config(empty.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains(sync::VAULT_CONFIG), "{err}");
}

#[test]
fn a_public_destination_is_refused() {
    let dir = vault("schema_version = \"1.0\"\nvisibility = \"public\"\n");
    let err = sync::load_vault_config(dir.path()).unwrap_err().to_string();
    assert!(err.contains("private 以外"), "{err}");
}

#[test]
fn a_misspelled_key_stops_the_sync_rather_than_warning() {
    // A warning would leave the misspelling in place and keep sending.
    let dir =
        vault("schema_version = \"1.0\"\nvisibility = \"private\"\nvisibilty = \"private\"\n");
    assert!(sync::load_vault_config(dir.path()).is_err());
}

#[test]
fn a_declaration_that_is_a_symbolic_link_is_not_followed() {
    let real = vault("schema_version = \"1.0\"\nvisibility = \"private\"\n");
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        real.path().join(sync::VAULT_CONFIG),
        dir.path().join(sync::VAULT_CONFIG),
    )
    .unwrap();

    // A link is something another repository can point at a permissive file;
    // the destination itself has to be the one that spoke.
    let err = sync::load_vault_config(dir.path()).unwrap_err().to_string();
    assert!(err.contains("シンボリックリンク"), "{err}");
}

#[test]
fn the_declaration_is_taken_from_the_nearest_ancestor() {
    let root = private_vault();
    let nested = root.path().join(REPO_ID).join("schelling");
    std::fs::create_dir_all(&nested).unwrap();
    let (found, config) = sync::load_vault_config(&nested).unwrap();
    assert_eq!(found, root.path());
    assert_eq!(config.compress_over_mib, 10.0);
}

// --- what is sent ------------------------------------------------------------

#[test]
fn a_public_run_sends_its_light_half_and_leaves_the_artifacts_behind() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    let receipt = sync::execute(&plan).unwrap().receipt;

    let stored: Vec<&str> = receipt
        .files
        .iter()
        .map(|f| f.stored_path.as_str())
        .collect();
    assert!(stored.contains(&"run.json"), "{stored:?}");
    assert!(stored.contains(&"config.json"), "{stored:?}");
    assert!(stored.contains(&"status.json"), "{stored:?}");
    assert!(stored.contains(&"metrics.csv"), "{stored:?}");
    assert!(stored.contains(&"manifest.csv"), "{stored:?}");
    assert!(stored.contains(&"events.jsonl"), "{stored:?}");
    // The heavy half stays where it is; `manifest.csv` already carries its
    // identity, so the aggregation copy does not need the bytes.
    assert!(
        !stored.iter().any(|p| p.starts_with("artifacts/")),
        "{stored:?}"
    );
    assert!(!plan.dest.join("artifacts").exists());

    assert_eq!(receipt.run_uid.as_deref(), Some(receipt.run_key.as_str()));
    assert_eq!(receipt.generation, 1);
    assert_eq!(receipt.verified, Some(true));
    assert_valid("sync", &read_json(&plan.dest.join(sync::RECEIPT)));
}

#[test]
fn the_source_run_is_untouched_by_being_synced() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let before = listing(&run);
    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    sync::execute(&plan).unwrap();

    // Syncing is a copy. A run that was sent has to look exactly like one that
    // was not, or the source stops being the record.
    assert_eq!(before, listing(&run));
}

/// Every file under a run directory, with its bytes, for comparison.
fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_run_that_did_not_declare_itself_public_is_held_back() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Internal);

    match plan_one(results.path(), &run, vault.path(), false) {
        Planned::Skipped { reason, .. } => assert!(reason.contains("internal"), "{reason}"),
        Planned::Send(_) => panic!("internal な run が送られました"),
    }
    // And nothing was created at the destination on the way to deciding that.
    assert!(!vault.path().join(REPO_ID).exists());

    sent(plan_one(results.path(), &run, vault.path(), true));
}

#[test]
fn a_run_that_contradicts_itself_is_not_preserved() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let mut config = read_json(&run.join("config.json"));
    config["parameters"]["rows"] = json!(99);
    std::fs::write(
        run.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    match plan_one(results.path(), &run, vault.path(), false) {
        Planned::Skipped { reason, .. } => assert!(reason.starts_with("verify"), "{reason}"),
        Planned::Send(_) => panic!("壊れた run が送られました"),
    }
}

#[test]
fn a_run_whose_repository_id_disagrees_is_an_error_not_a_guess() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let err = sync::plan_canonical(&run, "some-other-repo", vault.path(), &options(false))
        .unwrap_err()
        .to_string();
    assert!(err.contains("repo_id"), "{err}");
}

// --- the globs ---------------------------------------------------------------

fn run_with_globs(results: &Path, include: &[&str], exclude: &[&str]) -> PathBuf {
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
            .master_seed(42)
            .sync_include(include.iter().map(|s| s.to_string()))
            .sync_exclude(exclude.iter().map(|s| s.to_string())),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts/summary.csv"), "a,b\n1,2\n").unwrap();
    run.log_event(
        "observation",
        &json!({"unit_id": "u0", "t": 1.0, "t_unit": "step", "prompt": "secret"}),
    )
    .unwrap();
    run.finish().unwrap();
    dir
}

#[test]
fn an_excluded_file_does_not_leave_the_machine() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = run_with_globs(results.path(), &[], &["events.jsonl"]);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    assert!(!plan.files.iter().any(|f| f.path == "events.jsonl"));
    sync::execute(&plan).unwrap();
    assert!(!plan.dest.join("events.jsonl").exists());
}

#[test]
fn an_included_artifact_is_sent_although_artifacts_are_not() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = run_with_globs(results.path(), &["artifacts/*.csv"], &[]);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    assert!(plan.files.iter().any(|f| f.path == "artifacts/summary.csv"));
}

#[test]
fn exclusion_wins_wherever_the_two_globs_meet() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = run_with_globs(results.path(), &["artifacts/*"], &["artifacts/*.csv"]);

    // Between sending something that should not leave and holding something
    // back, the record errs on holding back.
    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    assert!(!plan.files.iter().any(|f| f.path.starts_with("artifacts/")));
}

#[test]
fn a_run_may_not_exclude_the_file_that_says_what_it_is() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = run_with_globs(results.path(), &[], &["run.json"]);

    let err = sync::plan_canonical(&run, REPO_ID, vault.path(), &options(false))
        .unwrap_err()
        .to_string();
    assert!(err.contains("run.json"), "{err}");
}

// --- re-syncing --------------------------------------------------------------

/// Appends a metric row to a finished run, as a resumed run would.
fn append_metric(run: &Path, line: &str) {
    let path = run.join("metrics.csv");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(line);
    std::fs::write(&path, text).unwrap();
}

#[test]
fn a_second_sync_counts_up_and_accepts_what_only_grew() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    assert_eq!(sync::execute(&plan).unwrap().receipt.generation, 1);

    let uid = read_json(&run.join("run.json"))["run_uid"]
        .as_str()
        .unwrap()
        .to_string();
    append_metric(&run, &format!("{uid},,,run,extra,1.0\n"));

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    let receipt = sync::execute(&plan).unwrap().receipt;
    assert_eq!(receipt.generation, 2);
    assert!(
        std::fs::read_to_string(plan.dest.join("metrics.csv"))
            .unwrap()
            .contains("extra")
    );
}

#[test]
fn a_second_sync_refuses_a_history_that_was_rewritten() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    sync::execute(&plan).unwrap();

    // The same length, and then some: a size comparison would wave this
    // through, and the earlier row would be gone from the record for good.
    let uid = read_json(&run.join("run.json"))["run_uid"]
        .as_str()
        .unwrap()
        .to_string();
    let text = std::fs::read_to_string(run.join("metrics.csv")).unwrap();
    let rewritten = text.replace("0.83", "0.11");
    assert_eq!(text.len(), rewritten.len());
    std::fs::write(
        run.join("metrics.csv"),
        rewritten + &format!("{uid},,,run,extra,1.0\n"),
    )
    .unwrap();

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    let err = sync::execute(&plan).unwrap_err().to_string();
    assert!(err.contains("追記のみ"), "{err}");
}

#[test]
fn a_second_sync_refuses_a_run_that_now_claims_something_else() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    sync::execute(&plan).unwrap();

    // Same `run_uid`, different content. One of the two is not what happened.
    let mut meta = read_json(&run.join("run.json"));
    meta["cli_args"] = json!(["--rows", "99"]);
    std::fs::write(
        run.join("run.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    let err = sync::execute(&plan).unwrap_err().to_string();
    assert!(err.contains("run.json"), "{err}");
}

#[test]
fn a_file_the_source_lost_stays_at_the_destination_and_is_reported() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = run_with_globs(results.path(), &[], &[]);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    sync::execute(&plan).unwrap();
    assert!(plan.dest.join("events.jsonl").is_file());

    std::fs::remove_file(run.join("events.jsonl")).unwrap();
    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    let synced = sync::execute(&plan).unwrap();

    // The aggregation copy exists because the source lives on one machine.
    // Deleting from it because the source lost a file would remove the last
    // copy of exactly the run that needed one — so it is reported, not deleted.
    assert_eq!(synced.left_behind, vec!["events.jsonl".to_string()]);
    assert!(plan.dest.join("events.jsonl").is_file());
    assert!(
        !synced
            .receipt
            .files
            .iter()
            .any(|f| f.path == "events.jsonl")
    );
}

// --- compression -------------------------------------------------------------

/// A vault that compresses almost everything, so the threshold is exercised.
fn compressing_vault() -> tempfile::TempDir {
    vault("schema_version = \"1.0\"\nvisibility = \"private\"\ncompress_over_mib = 0.0001\n")
}

#[test]
fn a_file_over_the_threshold_is_stored_compressed_and_reads_back() {
    let results = tempfile::tempdir().unwrap();
    let vault = compressing_vault();
    let (_, config) = sync::load_vault_config(vault.path()).unwrap();
    let run = finished_run(results.path(), Visibility::Public);

    // `run.json` is already past this vault's 0.0001 MiB threshold, so nothing
    // has to be enlarged — and enlarging an artifact would break `manifest.csv`.
    let source = std::fs::read(run.join("run.json")).unwrap();

    let plan = sent(
        sync::plan_canonical(
            &run,
            REPO_ID,
            vault.path(),
            &SyncOptions {
                allow_internal: false,
                compress_over_bytes: config.compress_over_bytes(),
            },
        )
        .unwrap(),
    );
    let stored = plan
        .files
        .iter()
        .find(|f| f.path == "run.json")
        .expect("run.json is planned");
    assert_eq!(stored.compression, Compression::Zstd);
    assert_eq!(stored.stored_path, "run.json.zst");

    let receipt = sync::execute(&plan).unwrap().receipt;
    assert!(plan.dest.join("run.json.zst").is_file());
    assert!(!plan.dest.join("run.json").exists());

    // The receipt carries both ends, because the compressed digest is the only
    // one that describes what is actually sitting in the repository.
    let recorded = receipt.files.iter().find(|f| f.path == "run.json").unwrap();
    assert_ne!(recorded.source.hash.value, recorded.stored.hash.value);
    assert_eq!(recorded.source.bytes, source.len() as u64);

    let decoded =
        zstd::decode_all(std::fs::File::open(plan.dest.join("run.json.zst")).unwrap()).unwrap();
    assert_eq!(decoded, source);
    assert_valid("sync", &read_json(&plan.dest.join(sync::RECEIPT)));
}

#[test]
fn crossing_the_threshold_leaves_only_one_form_behind() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    // Below the threshold first.
    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    sync::execute(&plan).unwrap();
    assert!(plan.dest.join("run.json").is_file());

    // Then above it. Two forms of the same file would leave a reader to decide
    // which one is the record.
    let plan = sent(
        sync::plan_canonical(
            &run,
            REPO_ID,
            vault.path(),
            &SyncOptions {
                allow_internal: false,
                compress_over_bytes: 1,
            },
        )
        .unwrap(),
    );
    sync::execute(&plan).unwrap();
    assert!(plan.dest.join("run.json.zst").is_file());
    assert!(!plan.dest.join("run.json").exists());
}

// --- by-slug -----------------------------------------------------------------

#[test]
fn the_readable_name_links_to_the_run_without_claiming_to_be_unique() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    sync::execute(&plan).unwrap();

    let slug = plan.run_slug.clone().unwrap();
    let uid = plan.run_uid.clone().unwrap();
    // A directory per name, one link per run inside it: `by-slug/<slug>` is not
    // itself a link, because the name it holds is not unique.
    let link = plan
        .dest
        .parent()
        .unwrap()
        .join("by-slug")
        .join(&slug)
        .join(&uid);
    assert!(link.symlink_metadata().is_ok(), "{}", link.display());
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(&plan.dest).unwrap()
    );
}

// --- legacy ------------------------------------------------------------------

/// A run directory in the shape the repositories used before this specification.
fn legacy_run(results: &Path) -> PathBuf {
    let dir = results.join("schelling").join("main_20240115_101500");
    std::fs::create_dir_all(dir.join("snapshots")).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"rows": 13}"#).unwrap();
    std::fs::write(dir.join("metrics.csv"), "t,segregation\n0,0.31\n").unwrap();
    std::fs::write(dir.join("figure.png"), vec![0u8; 2048]).unwrap();
    // A per-step grid dump: the heavy half of the record, written as CSV.
    std::fs::write(dir.join("snapshots/step_00000.csv"), "a,b\n1,0\n").unwrap();
    dir
}

#[test]
fn a_legacy_grid_dump_stays_behind_even_though_it_is_a_csv() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let dir = legacy_run(results.path());

    let plan = sent(
        sync::plan_legacy(results.path(), &dir, REPO_ID, vault.path(), &options(true)).unwrap(),
    );

    // §1.4 names `snapshots/` among the directories that never travel. Deciding
    // by extension alone shipped them, because a grid dump happens to be a CSV:
    // one real repository sent 745 files where it should have sent 30.
    assert!(
        !plan.files.iter().any(|f| f.path.starts_with("snapshots/")),
        "{:?}",
        plan.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert!(plan.files.iter().any(|f| f.path == "metrics.csv"));
}

#[test]
fn a_legacy_run_travels_with_its_key_because_the_destination_cannot_derive_it() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let dir = legacy_run(results.path());

    let plan = sent(
        sync::plan_legacy(results.path(), &dir, REPO_ID, vault.path(), &options(true)).unwrap(),
    );
    let receipt = sync::execute(&plan).unwrap().receipt;

    assert_eq!(receipt.run_uid, None);
    // The key is built from the path below the source `results/`, which the
    // destination layout no longer shows: the receipt is where it survives.
    assert_eq!(
        receipt.run_key,
        format!("legacy:{REPO_ID}:schelling/main_20240115_101500")
    );
    // Never examined is not the same as examined and passed.
    assert_eq!(receipt.verified, None);

    let stored: Vec<&str> = receipt
        .files
        .iter()
        .map(|f| f.stored_path.as_str())
        .collect();
    assert_eq!(stored, vec!["config.json", "metrics.csv"]);
    assert!(!plan.dest.join("figure.png").exists());
    assert_valid("sync", &read_json(&plan.dest.join(sync::RECEIPT)));
}

#[test]
fn a_legacy_run_is_held_back_by_default_because_it_declared_nothing() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let dir = legacy_run(results.path());

    match sync::plan_legacy(results.path(), &dir, REPO_ID, vault.path(), &options(false)).unwrap() {
        Planned::Skipped { reason, .. } => assert!(reason.contains("visibility"), "{reason}"),
        Planned::Send(_) => panic!("何も宣言していない run が送られました"),
    }
}

#[test]
fn one_scan_finds_both_kinds_and_counts_each_once() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    finished_run(results.path(), Visibility::Public);
    legacy_run(results.path());

    let planned = sync::plan_all(results.path(), REPO_ID, vault.path(), &options(true)).unwrap();
    let plans: Vec<_> = planned.into_iter().map(sent).collect();
    assert_eq!(plans.len(), 2);
    assert_eq!(plans.iter().filter(|p| p.run_uid.is_some()).count(), 1);
    assert_eq!(plans.iter().filter(|p| p.run_uid.is_none()).count(), 1);
}

// --- the command -------------------------------------------------------------

#[test]
fn a_killed_run_travels_as_the_failure_it_is() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = killed_run(results.path());

    // The record of a failure is still a record, and it is the only copy: the
    // source `results/` is gitignored. Holding it back over an artifact that
    // `sync` was never going to send stopped an entire repository from being
    // preserved, once a day, until a person deleted the run (MYTASK-3202).
    let plan = sent(plan_one(results.path(), &run, vault.path(), false));
    let names: Vec<&str> = plan.files.iter().map(|f| f.path.as_str()).collect();
    assert!(names.contains(&"status.json"), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("artifacts/")),
        "the heavy half stays where it is: {names:?}"
    );
}

#[test]
fn a_result_carrying_an_unrecorded_artifact_is_still_held_back() {
    let results = tempfile::tempdir().unwrap();
    let vault = private_vault();
    let run = finished_run(results.path(), Visibility::Public);

    // The other half of the same decision. A run that sealed and claims to be a
    // result answers for everything under `artifacts/`, and a file that is not
    // in its manifest appeared after the run ended.
    std::fs::write(run.join("artifacts/uninvited.csv"), "1\n").unwrap();

    match plan_one(results.path(), &run, vault.path(), false) {
        Planned::Skipped { reason, .. } => assert!(reason.contains("manifest.csv"), "{reason}"),
        Planned::Send(_) => panic!("記録に無い生成物を抱えた result が送られました"),
    }
}
