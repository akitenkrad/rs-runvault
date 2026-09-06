//! `status.json` is re-checked against its schema when it is read (MYTASK-3203).
//!
//! A schema runs at write time and never again. Serde carries most of the file
//! back — `RunStatus` is `deny_unknown_fields` with typed fields, so a missing
//! key, a wrong type and an unknown key are refused before `verify` sees it —
//! but a `const`, a `format`, a `minimum` and a `minLength` are not things a
//! Rust type can say, and those passed unexamined.
//!
//! Two kinds of test live here. The first kind corrupts one value in a
//! well-formed `status.json` and asserts `verify` names the field. The second is
//! `the_schema_carries_no_constraint_this_crate_ignores`, which pins every
//! constraint the schema declares: mirroring the schema in Rust is duplication,
//! and that test is what keeps the duplication loud instead of silent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use runvault::meta::Origin;
use runvault::{Run, RunOptions};
use serde_json::{Value, json};

/// A finished run with nothing in it but the files every run has.
fn finished_run(results: &Path) -> PathBuf {
    let run = Run::start(
        RunOptions::new("e", "main")
            .repo_id("r")
            .domain("other")
            .origin(Origin::Manual)
            .results_root(results)
            .parameters(&json!({}))
            .unwrap(),
    )
    .unwrap();
    let dir = run.dir().to_path_buf();
    run.finish().unwrap();
    dir
}

fn read_status(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("status.json")).unwrap()).unwrap()
}

fn write_status(dir: &Path, status: &Value) {
    std::fs::write(
        dir.join("status.json"),
        serde_json::to_string_pretty(status).unwrap(),
    )
    .unwrap();
}

/// Rewrites `status.json` through `edit`, then asserts the message names `field`.
#[track_caller]
fn rejected_for(edit: impl FnOnce(&mut Value), field: &str) {
    let results = tempfile::tempdir().unwrap();
    let dir = finished_run(results.path());
    let mut status = read_status(&dir);
    edit(&mut status);
    write_status(&dir, &status);

    let err = runvault::verify::shallow(&dir)
        .expect_err("the edited status.json should have been refused")
        .to_string();
    assert!(err.contains(field), "message does not name {field}: {err}");
}

#[test]
fn an_untouched_status_still_passes() {
    let results = tempfile::tempdir().unwrap();
    let dir = finished_run(results.path());
    runvault::verify::shallow(&dir).unwrap();
    runvault::verify::deep(&dir).unwrap();
}

#[test]
fn a_status_written_to_another_version_of_the_schema_is_refused() {
    rejected_for(|s| s["schema_version"] = json!("1.1"), "schema_version");
}

#[test]
fn a_timestamp_that_is_not_a_date_time_is_refused() {
    rejected_for(
        |s| s["started_at"] = json!("2026-09-06 17:42:50"),
        "started_at",
    );
    rejected_for(|s| s["finished_at"] = json!("yesterday"), "finished_at");
}

#[test]
fn a_negative_duration_is_refused() {
    rejected_for(|s| s["duration_sec"] = json!(-1.5), "duration_sec");
}

#[test]
fn an_error_with_an_empty_kind_or_message_is_refused() {
    let failed = |kind: &str, message: &str| {
        let (kind, message) = (kind.to_string(), message.to_string());
        move |s: &mut Value| {
            s["state"] = json!("failed");
            s["exit_code"] = json!(null);
            s["error"] = json!({"kind": kind, "message": message});
        }
    };
    rejected_for(failed("", "the process stopped"), "error.kind");
    rejected_for(failed("dropped", ""), "error.message");
}

#[test]
fn a_run_uid_that_is_not_a_ulid_is_refused() {
    // Both spellings have to move together, or the cross-file check fires first
    // and says nothing about the grammar.
    rejected_for(
        |s| s["run_uid"] = json!("not-a-ulid"),
        "run.json", // the equality check catches this one
    );
    let results = tempfile::tempdir().unwrap();
    let dir = finished_run(results.path());
    for file in ["run.json", "status.json"] {
        let path = dir.join(file);
        let mut value: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["run_uid"] = json!("not-a-ulid");
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    }
    let err = runvault::verify::shallow(&dir).unwrap_err().to_string();
    assert!(err.contains("run_uid"), "{err}");
}

#[test]
fn a_finished_run_may_not_carry_an_error_or_a_non_zero_exit() {
    rejected_for(
        |s| s["error"] = json!({"kind": "verify", "message": "…"}),
        "error",
    );
    rejected_for(|s| s["exit_code"] = json!(1), "exit_code");
}

#[test]
fn a_collision_index_below_two_is_refused() {
    // The first run of a condition takes the bare slug; a `-N` suffix starts at 2.
    rejected_for(|s| s["collision_index"] = json!(1), "collision_index");
}

#[test]
fn a_failed_run_still_has_to_say_why() {
    rejected_for(
        |s| {
            s["state"] = json!("failed");
            s["exit_code"] = json!(null);
            s["error"] = json!(null);
        },
        "error",
    );
}

/// A run killed before it could seal keeps verifying (commit `13796d6`).
///
/// It has no `manifest.csv`, and the artifacts it left behind are exempt from
/// the unrecorded-file check. The new value-level checks must not take that back.
#[test]
fn a_run_that_never_sealed_still_verifies() {
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
    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts/partial.csv"), "t,value\n0,0.31\n").unwrap();
    run.fail(
        "dropped",
        "the process stopped before it could seal the run",
    )
    .unwrap();

    assert!(!dir.join("manifest.csv").exists());
    runvault::verify::shallow(&dir).unwrap();
    runvault::verify::deep(&dir).unwrap();
}

// ---------------------------------------------------------------------------
// The inventory that keeps the mirror honest.
// ---------------------------------------------------------------------------

/// The keywords that constrain a *value*. `description`, `$comment` and
/// `additionalProperties` say nothing a reader has to enforce, so they are out.
const CONSTRAINT_KEYWORDS: &[&str] = &[
    "$ref",
    "const",
    "enum",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "format",
    "maxItems",
    "maxLength",
    "maximum",
    "minItems",
    "minLength",
    "minimum",
    "multipleOf",
    "pattern",
    "required",
    "type",
    "uniqueItems",
];

/// Every constraint `schema/v1/status.json` declares, as `<pointer> <keyword>=<value>`.
///
/// Each line is enforced by something. Most `type` and `required` lines are
/// enforced by Serde, because `RunStatus` is `deny_unknown_fields` with typed
/// fields; the rest are `verify::check_status_schema`, which mirrors them by
/// hand. Reading the JSON Schema at run time instead would put the whole of
/// `jsonschema` and 44 further crates into a library that 30 repositories pin
/// by revision, which is not a trade this constraint set is worth.
///
/// **When this test fails, the schema changed.** Decide what enforces the new
/// line — Serde, or a check in `verify::check_status_schema` — add it, and then
/// add the line here. Do not add the line alone.
const DECLARED_CONSTRAINTS: &[&str] = &[
    r#"# required=["schema_version","run_uid","state","started_at","finished_at","duration_sec"]"#,
    r#"# type="object""#,
    r#"#/allOf/0/if required=["state"]"#,
    r#"#/allOf/0/if/properties/state const="failed""#,
    r#"#/allOf/0/then required=["error"]"#,
    r#"#/allOf/0/then/properties/error type="object""#,
    r#"#/allOf/1/if required=["state"]"#,
    r#"#/allOf/1/if/properties/state const="finished""#,
    r#"#/allOf/1/then/properties/error type="null""#,
    r#"#/allOf/1/then/properties/exit_code enum=[0,null]"#,
    r#"#/properties/collision_index minimum=2"#,
    r#"#/properties/collision_index type=["integer","null"]"#,
    r#"#/properties/counts type=["object","null"]"#,
    r#"#/properties/counts/properties/artifacts minimum=0"#,
    r#"#/properties/counts/properties/artifacts type="integer""#,
    r#"#/properties/counts/properties/events minimum=0"#,
    r#"#/properties/counts/properties/events type="integer""#,
    r#"#/properties/counts/properties/metrics minimum=0"#,
    r#"#/properties/counts/properties/metrics type="integer""#,
    r#"#/properties/duration_sec minimum=0"#,
    r#"#/properties/duration_sec type="number""#,
    r#"#/properties/error required=["kind","message"]"#,
    r#"#/properties/error type=["object","null"]"#,
    r#"#/properties/error/properties/kind $ref="common.json#/$defs/nonempty""#,
    r#"#/properties/error/properties/message $ref="common.json#/$defs/nonempty""#,
    r#"#/properties/exit_code type=["integer","null"]"#,
    r#"#/properties/finished_at $ref="common.json#/$defs/timestamp""#,
    r#"#/properties/run_uid $ref="common.json#/$defs/run_uid""#,
    r#"#/properties/schema_version const="1.0""#,
    r#"#/properties/schema_version type="string""#,
    r#"#/properties/started_at $ref="common.json#/$defs/timestamp""#,
    r#"#/properties/state enum=["finished","failed"]"#,
    r#"common.json#/$defs/nonempty minLength=1"#,
    r#"common.json#/$defs/nonempty type="string""#,
    r#"common.json#/$defs/run_uid pattern="^[0-7][0-9A-HJKMNP-TV-Z]{25}$""#,
    r#"common.json#/$defs/run_uid type="string""#,
    r#"common.json#/$defs/timestamp format="date-time""#,
    r#"common.json#/$defs/timestamp type="string""#,
];

fn schema_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", "schema", "v1"]
        .iter()
        .collect()
}

fn load(file: &str) -> Value {
    let path = schema_dir().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("schema is valid JSON")
}

/// Collects `<pointer> <keyword>=<value>` for every constraint under `node`.
fn collect(node: &Value, pointer: &str, out: &mut BTreeSet<String>) {
    match node {
        Value::Object(map) => {
            for keyword in CONSTRAINT_KEYWORDS {
                if let Some(value) = map.get(*keyword) {
                    out.insert(format!("{pointer} {keyword}={value}"));
                }
            }
            for (key, value) in map {
                // A keyword's own value is recorded, not walked into; prose is skipped.
                if CONSTRAINT_KEYWORDS.contains(&key.as_str())
                    || matches!(
                        key.as_str(),
                        "description" | "$comment" | "title" | "$id" | "$schema"
                    )
                {
                    continue;
                }
                collect(value, &format!("{pointer}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (i, value) in items.iter().enumerate() {
                collect(value, &format!("{pointer}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn the_schema_carries_no_constraint_this_crate_ignores() {
    let mut found = BTreeSet::new();
    collect(&load("status.json"), "#", &mut found);

    // The shared definitions carry the constraint that `$ref` only points at.
    let common = load("common.json");
    let referenced: BTreeSet<String> = found
        .iter()
        .filter_map(|entry| entry.split_once(" $ref=").map(|(_, target)| target))
        .filter_map(|target| target.trim_matches('"').rsplit('/').next())
        .map(str::to_string)
        .collect();
    for name in referenced {
        let node = &common["$defs"][&name];
        assert!(!node.is_null(), "common.json has no $defs/{name}");
        collect(node, &format!("common.json#/$defs/{name}"), &mut found);
    }

    let declared: BTreeSet<String> = DECLARED_CONSTRAINTS.iter().map(|s| s.to_string()).collect();
    let added: Vec<_> = found.difference(&declared).collect();
    let removed: Vec<_> = declared.difference(&found).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "schema/v1/status.json no longer matches DECLARED_CONSTRAINTS.\n\
         Added by the schema (nothing enforces these yet): {added:#?}\n\
         Gone from the schema (the checks for these are now dead): {removed:#?}\n\
         Add or drop the check in verify::check_status_schema first, then this list."
    );
}
