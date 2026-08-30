//! Keeps the Rust types and `schema/v1/*.json` from drifting apart.
//!
//! The schemas were written by hand in Phase 0 and are the specification; the
//! Rust types only mirror them. A generated schema cannot be compared to them
//! byte for byte — the hand-written ones carry conditional requirements
//! (`if`/`then`) that no derive can express — so what is compared is the part
//! that *can* drift silently: which keys a type writes, and whether every key
//! the schema demands exists in the type at all.
//!
//! Run with `cargo test --features schema-gen`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use schemars::{JsonSchema, schema_for};
use serde_json::Value;

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

/// Follows a JSON Pointer, then a `$ref` if the node is one.
fn node<'a>(root: &'a Value, pointer: &str) -> &'a Value {
    root.pointer(pointer)
        .unwrap_or_else(|| panic!("{pointer} is missing from the schema"))
}

/// The property names an object node declares.
fn properties(node: &Value) -> BTreeSet<String> {
    node.get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// The names an object node requires unconditionally.
fn required(node: &Value) -> BTreeSet<String> {
    node.get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolves `#/$defs/...` references inside a generated schema.
fn resolve<'a>(generated: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let pointer = reference.trim_start_matches('#');
            generated
                .pointer(pointer)
                .unwrap_or_else(|| panic!("{reference} is missing"))
        }
        None => node,
    }
}

/// Compares one Rust type against one node of a hand-written schema.
#[track_caller]
fn assert_parity<T: JsonSchema>(label: &str, hand_written: &Value) {
    let generated = serde_json::to_value(schema_for!(T)).expect("schemars emits JSON");
    let generated_root = resolve(&generated, &generated);

    let from_rust = properties(generated_root);
    let from_schema = properties(hand_written);
    assert_eq!(
        from_rust, from_schema,
        "{label}: the type writes {from_rust:?} but the schema declares {from_schema:?}"
    );

    // Whether a key is *required* differs by construction: the schemas make
    // several `null`-able keys mandatory, which no `Option<T>` can express. What
    // must hold is that nothing the schema demands is missing from the type.
    let missing: Vec<_> = required(hand_written)
        .difference(&from_rust)
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "{label}: the schema requires {missing:?}, which the type lacks"
    );
}

#[test]
fn run_json_matches_the_types_that_write_it() {
    let run = load("run.json");
    assert_parity::<runvault::meta::RunMeta>("run.json", &run);
    assert_parity::<runvault::meta::Code>("run.json /code", node(&run, "/properties/code"));
    assert_parity::<runvault::meta::Env>("run.json /env", node(&run, "/properties/env"));
    assert_parity::<runvault::meta::Rng>("run.json /rng", node(&run, "/properties/rng"));
    assert_parity::<runvault::meta::Llm>("run.json /llm", node(&run, "/properties/llm"));
    assert_parity::<runvault::meta::Dataset>(
        "run.json /data[]",
        node(&run, "/properties/data/items"),
    );
    assert_parity::<runvault::meta::Lock>(
        "run.json /code/locks[]",
        node(&run, "/properties/code/properties/locks/items"),
    );
    assert_parity::<runvault::meta::Lineage>(
        "run.json /lineage",
        node(&run, "/properties/lineage"),
    );
    assert_parity::<runvault::meta::Research>(
        "run.json /research",
        node(&run, "/properties/research"),
    );
    assert_parity::<runvault::meta::Work>(
        "run.json /research/work",
        node(&run, "/properties/research/properties/work"),
    );
    assert_parity::<runvault::meta::Target>(
        "run.json /research/targets[]",
        node(&run, "/properties/research/properties/targets/items"),
    );
    assert_parity::<runvault::meta::UpstreamImpl>(
        "run.json /research/upstream_impl",
        node(&run, "/properties/research/properties/upstream_impl"),
    );
}

#[test]
fn the_shared_hash_definition_matches() {
    assert_parity::<runvault::meta::Hash>(
        "common.json hash",
        node(&load("common.json"), "/$defs/hash"),
    );
}

#[test]
fn config_json_matches_the_types_that_write_it() {
    let config = load("config.json");
    assert_parity::<runvault::config::ConfigEnvelope>("config.json", &config);
    assert_parity::<runvault::config::RunvaultBlock>(
        "config.json /runvault",
        node(&config, "/properties/runvault"),
    );
    assert_parity::<runvault::config::Determinism>(
        "config.json /runvault/determinism",
        node(&config, "/properties/runvault/properties/determinism"),
    );
}

#[test]
fn status_json_matches_the_types_that_write_it() {
    let status = load("status.json");
    assert_parity::<runvault::status::RunStatus>("status.json", &status);
    assert_parity::<runvault::status::StatusError>(
        "status.json /error",
        node(&status, "/properties/error"),
    );
    assert_parity::<runvault::status::Counts>(
        "status.json /counts",
        node(&status, "/properties/counts"),
    );
}

/// The string values an enumeration allows, however the schema spells it.
///
/// A hand-written schema uses `enum`; `schemars` emits `oneOf` of `const`s.
fn enum_values(node: &Value) -> BTreeSet<String> {
    if let Some(items) = node.get("enum").and_then(Value::as_array) {
        return items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
    }
    node.get("oneOf")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.get("const").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[track_caller]
fn assert_enum_parity<T: JsonSchema>(label: &str, hand_written: &Value) {
    let generated = serde_json::to_value(schema_for!(T)).expect("schemars emits JSON");
    let from_rust = enum_values(&generated);
    let from_schema = enum_values(hand_written);
    assert!(
        !from_schema.is_empty(),
        "{label}: the schema declares no values"
    );
    assert_eq!(
        from_rust, from_schema,
        "{label}: the type offers {from_rust:?} but the schema allows {from_schema:?}"
    );
}

#[test]
fn the_enumerations_offer_exactly_the_values_the_schema_allows() {
    let run = load("run.json");
    assert_enum_parity::<runvault::meta::Origin>("origin", node(&run, "/properties/origin"));
    assert_enum_parity::<runvault::meta::Visibility>(
        "visibility",
        node(&run, "/properties/visibility"),
    );
    assert_enum_parity::<runvault::meta::TargetKind>(
        "research.targets[].kind",
        node(
            &run,
            "/properties/research/properties/targets/items/properties/kind",
        ),
    );
    assert_enum_parity::<runvault::meta::LockKind>(
        "code.locks[].kind",
        node(
            &run,
            "/properties/code/properties/locks/items/properties/kind",
        ),
    );
    assert_enum_parity::<runvault::meta::Algorithm>(
        "hash.algorithm",
        node(&load("common.json"), "/$defs/hash/properties/algorithm"),
    );
    assert_enum_parity::<runvault::status::State>(
        "state",
        node(&load("status.json"), "/properties/state"),
    );
}
