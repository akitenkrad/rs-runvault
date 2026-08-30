//! Checks the Rust implementation against `schema/v1/testvectors/`.
//!
//! The vectors are produced by a second implementation written from the design
//! note (`tools/gen_testvectors.py`). A disagreement here means one of the two
//! is wrong; it is not a reason to regenerate the vectors.

use std::path::PathBuf;

use runvault::canonical::{blake3_hex, canonicalize, join_lp};
use runvault::config::{ConfigEnvelope, Exclusions};
use runvault::hash::{config_hash, env_hash, execution_hash};
use runvault::meta::{Code, Dataset, Lock};
use serde_json::Value;

fn vectors(name: &str) -> Value {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "schema", "v1", "testvectors", name]
        .iter()
        .collect();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("test vectors are valid JSON")
}

fn cases(name: &str) -> Vec<Value> {
    vectors(name)["cases"].as_array().expect("cases").clone()
}

#[test]
fn canonicalization_matches_the_vectors() {
    let cases = cases("canonicalize.json");
    assert!(cases.len() >= 15, "the vectors lost cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let canonical = canonicalize(&case["value"]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(canonical, case["canonical"].as_str().unwrap(), "{name}");
        assert_eq!(blake3_hex(canonical.as_bytes()), case["blake3"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn length_prefixed_joining_matches_the_vectors() {
    for case in cases("length_prefix.json") {
        let name = case["name"].as_str().unwrap();
        let inputs: Vec<String> = case["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let joined = join_lp(inputs.iter().map(|s| s.as_bytes()));
        assert_eq!(String::from_utf8(joined.clone()).unwrap(), case["joined"].as_str().unwrap(), "{name}");
        assert_eq!(blake3_hex(&joined), case["blake3"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn the_three_hashes_match_the_vectors() {
    let cases = cases("hashes.json");
    assert!(cases.len() >= 3, "the vectors lost cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let config: ConfigEnvelope = serde_json::from_value(case["config"].clone())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let data: Vec<Dataset> = serde_json::from_value(case["data"].clone())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let code: Option<Code> = code_of(&case["code"]);
        let env = &case["env"];
        let locks: Vec<Lock> = serde_json::from_value(env["locks"].clone()).unwrap();
        let expect = &case["expect"];

        let e = env_hash(
            env["os"].as_str().unwrap(),
            env["arch"].as_str().unwrap(),
            env["rustc_version"].as_str(),
            env["python_version"].as_str(),
            &locks,
        );
        assert_eq!(e, expect["env_hash"].as_str().unwrap(), "{name}: env_hash");

        let exclusions = Exclusions::resolve(&config.runvault, &config.parameters)
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        let pruned = runvault::pointer::prune(&config.parameters, &exclusions.removed_from_config());
        assert_eq!(
            canonicalize(&pruned).unwrap(),
            expect["config_canonical"].as_str().unwrap(),
            "{name}: config_canonical"
        );

        let cfg = config_hash(&config.parameters, &exclusions, &data).unwrap();
        assert_eq!(cfg, expect["config_hash"].as_str().unwrap(), "{name}: config_hash");

        let ex = execution_hash(&cfg, &config.parameters, &exclusions, code.as_ref(), &e).unwrap();
        assert_eq!(ex, expect["execution_hash"].as_str().unwrap(), "{name}: execution_hash");
    }
}

/// The vectors write `code` the way `run.json` does, without the optional keys.
fn code_of(value: &Value) -> Option<Code> {
    let obj = value.as_object()?;
    Some(Code {
        git_remote: None,
        git_branch: None,
        git_commit: obj["git_commit"].as_str().unwrap().to_string(),
        git_dirty: obj["git_dirty"].as_bool().unwrap(),
        dirty_hash: obj
            .get("dirty_hash")
            .and_then(|h| serde_json::from_value(h.clone()).ok()),
        repo_relpath: None,
        locks: vec![],
    })
}
