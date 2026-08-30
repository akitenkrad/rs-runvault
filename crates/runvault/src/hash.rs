//! The three hashes, defined as a stack so no rule is written twice.
//!
//! `env_hash` feeds `execution_hash`; `config_hash` feeds `execution_hash`.
//! Every input is length-prefixed, so a value containing the separator can never
//! be mistaken for a different list of inputs (design note §3.3).

use serde_json::Value;

use crate::canonical::{blake3_hex, canonicalize, join_lp, push_lp};
use crate::config::Exclusions;
use crate::error::Result;
use crate::meta::{Code, Dataset, Lock};
use crate::pointer::prune;

/// BLAKE3 over the machine-independent parts of the environment.
///
/// `host` is deliberately left out: the question is "did the environment differ",
/// not "was it the same box", so two machines with the same OS and the same
/// dependencies get the same `env_hash`.
pub fn env_hash(
    os: &str,
    arch: &str,
    rustc_version: Option<&str>,
    python_version: Option<&str>,
    locks: &[Lock],
) -> String {
    let mut sorted: Vec<&Lock> = locks.iter().collect();
    sorted.sort_by(|a, b| (a.kind_str(), &a.file).cmp(&(b.kind_str(), &b.file)));

    let mut locks_blob = Vec::new();
    for lock in sorted {
        push_lp(&mut locks_blob, lock.kind_str().as_bytes());
        push_lp(&mut locks_blob, lock.file.as_bytes());
        push_lp(&mut locks_blob, lock.hash.algorithm_str().as_bytes());
        push_lp(&mut locks_blob, lock.hash.value.as_bytes());
    }

    let bytes = join_lp([
        os.as_bytes(),
        arch.as_bytes(),
        rustc_version.unwrap_or("").as_bytes(),
        python_version.unwrap_or("").as_bytes(),
        locks_blob.as_slice(),
    ]);
    blake3_hex(&bytes)
}

/// The identity of the data the run used.
///
/// Every field is concatenated rather than one being picked as representative:
/// picking one would bundle runs that differ only in `split` or `version` as the
/// same condition, and changing the split is a change of condition.
pub fn data_identity(data: &[Dataset]) -> Vec<u8> {
    let mut sorted: Vec<&Dataset> = data.iter().collect();
    sorted.sort_by(|a, b| (&a.role, &a.name).cmp(&(&b.role, &b.name)));

    let mut blob = Vec::new();
    for d in sorted {
        let (algorithm, value) = match &d.hash {
            Some(h) => (h.algorithm_str(), h.value.as_str()),
            None => ("", ""),
        };
        push_lp(&mut blob, d.role.as_bytes());
        push_lp(&mut blob, d.name.as_bytes());
        push_lp(&mut blob, d.dataset_id.as_deref().unwrap_or("").as_bytes());
        push_lp(&mut blob, d.version.as_deref().unwrap_or("").as_bytes());
        push_lp(&mut blob, d.split.as_deref().unwrap_or("").as_bytes());
        push_lp(&mut blob, algorithm.as_bytes());
        push_lp(&mut blob, value.as_bytes());
        push_lp(&mut blob, d.hash_scope_str().as_bytes());
        push_lp(&mut blob, d.uri.as_deref().unwrap_or("").as_bytes());
        push_lp(
            &mut blob,
            d.n.map(|n| n.to_string()).unwrap_or_default().as_bytes(),
        );
    }
    blob
}

/// BLAKE3 over the experimental condition: the filtered parameters plus the data.
pub fn config_hash(
    parameters: &Value,
    exclusions: &Exclusions,
    data: &[Dataset],
) -> Result<String> {
    let pruned = prune(parameters, &exclusions.removed_from_config());
    let canonical = canonicalize(&pruned)?;
    let identity = data_identity(data);
    Ok(blake3_hex(&join_lp([
        canonical.as_bytes(),
        identity.as_slice(),
    ])))
}

/// The seeds, as the byte string `execution_hash` takes as its second input.
fn seed_blob(parameters: &Value, exclusions: &Exclusions) -> Result<Vec<u8>> {
    let mut blob = Vec::new();
    for pointer in exclusions.ordered_seeds() {
        let value = pointer.resolve(parameters).cloned().unwrap_or(Value::Null);
        push_lp(&mut blob, pointer.raw().as_bytes());
        push_lp(&mut blob, canonicalize(&value)?.as_bytes());
    }
    Ok(blob)
}

/// BLAKE3 over the condition, the seeds, the code and the environment.
///
/// Hashing only the condition would mark a run as "already done" after the code
/// that produced it was edited, and a sweep would skip re-running it.
///
/// When `code` is `None` (`origin` is `manual` or `external`) its three inputs are
/// still concatenated, as zero-length inputs: dropping them would change the
/// number of inputs and could reproduce another run's byte string.
pub fn execution_hash(
    config_hash: &str,
    parameters: &Value,
    exclusions: &Exclusions,
    code: Option<&Code>,
    env_hash: &str,
) -> Result<String> {
    let seeds = seed_blob(parameters, exclusions)?;
    let (commit, dirty, dirty_hash) = match code {
        Some(c) => (
            c.git_commit.as_str(),
            if c.git_dirty { "true" } else { "false" },
            c.dirty_hash
                .as_ref()
                .map(|h| h.value.as_str())
                .unwrap_or(""),
        ),
        None => ("", "", ""),
    };

    Ok(blake3_hex(&join_lp([
        config_hash.as_bytes(),
        seeds.as_slice(),
        commit.as_bytes(),
        dirty.as_bytes(),
        dirty_hash.as_bytes(),
        env_hash.as_bytes(),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Determinism, RunvaultBlock};
    use crate::meta::{Hash, HashScope};
    use serde_json::json;

    fn exclusions(block: &RunvaultBlock, params: &Value) -> Exclusions {
        Exclusions::resolve(block, params).unwrap()
    }

    fn block(hash_exclude: &[&str], seeds: &[&str], invariant: &[&str]) -> RunvaultBlock {
        RunvaultBlock {
            hash_exclude: hash_exclude.iter().map(|s| s.to_string()).collect(),
            seed_pointers: seeds.iter().map(|s| s.to_string()).collect(),
            determinism: Determinism {
                invariant_to: invariant.iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        }
    }

    fn dataset(role: &str, name: &str) -> Dataset {
        Dataset {
            role: role.into(),
            name: name.into(),
            dataset_id: Some(format!("{name}@v1")),
            version: None,
            hash: None,
            hash_scope: None,
            n: None,
            uri: None,
            split: None,
        }
    }

    #[test]
    fn output_dir_does_not_change_the_condition() {
        // The real schelling1971 config carries output_dir, which changes every run.
        let b = block(&["/output_dir"], &["/seed"], &[]);
        let a = json!({"rows": 13, "seed": 42, "output_dir": "results/a"});
        let z = json!({"rows": 13, "seed": 42, "output_dir": "results/z"});
        assert_eq!(
            config_hash(&a, &exclusions(&b, &a), &[]).unwrap(),
            config_hash(&z, &exclusions(&b, &z), &[]).unwrap()
        );
    }

    #[test]
    fn a_replicate_shares_the_condition_but_not_the_execution() {
        let b = block(&[], &["/seed"], &[]);
        let a = json!({"rows": 13, "seed": 1});
        let z = json!({"rows": 13, "seed": 2});
        let (ea, ez) = (exclusions(&b, &a), exclusions(&b, &z));
        let (ca, cz) = (
            config_hash(&a, &ea, &[]).unwrap(),
            config_hash(&z, &ez, &[]).unwrap(),
        );
        assert_eq!(ca, cz);
        assert_ne!(
            execution_hash(&ca, &a, &ea, None, "env").unwrap(),
            execution_hash(&cz, &z, &ez, None, "env").unwrap()
        );
    }

    #[test]
    fn changing_only_the_split_changes_the_condition() {
        let b = RunvaultBlock::default();
        let params = json!({"rows": 13});
        let ex = exclusions(&b, &params);
        let mut train = dataset("train", "cicids2017");
        let base = config_hash(&params, &ex, std::slice::from_ref(&train)).unwrap();
        train.split = Some("day1-4".into());
        let split = config_hash(&params, &ex, std::slice::from_ref(&train)).unwrap();
        assert_ne!(base, split);
    }

    #[test]
    fn data_order_does_not_matter() {
        let b = RunvaultBlock::default();
        let params = json!({});
        let ex = exclusions(&b, &params);
        let forward = [dataset("eval", "b"), dataset("train", "a")];
        let backward = [dataset("train", "a"), dataset("eval", "b")];
        assert_eq!(
            config_hash(&params, &ex, &forward).unwrap(),
            config_hash(&params, &ex, &backward).unwrap()
        );
    }

    #[test]
    fn no_data_and_one_dataset_differ() {
        let b = RunvaultBlock::default();
        let params = json!({});
        let ex = exclusions(&b, &params);
        assert_ne!(
            config_hash(&params, &ex, &[]).unwrap(),
            config_hash(&params, &ex, &[dataset("train", "a")]).unwrap()
        );
    }

    #[test]
    fn a_declared_invariant_key_leaves_the_condition_alone() {
        let b = block(&[], &[], &["/threads"]);
        let a = json!({"rows": 13, "threads": 1});
        let z = json!({"rows": 13, "threads": 8});
        assert_eq!(
            config_hash(&a, &exclusions(&b, &a), &[]).unwrap(),
            config_hash(&z, &exclusions(&b, &z), &[]).unwrap()
        );
    }

    #[test]
    fn threads_change_the_condition_unless_declared_invariant() {
        let b = RunvaultBlock::default();
        let a = json!({"rows": 13, "threads": 1});
        let z = json!({"rows": 13, "threads": 8});
        assert_ne!(
            config_hash(&a, &exclusions(&b, &a), &[]).unwrap(),
            config_hash(&z, &exclusions(&b, &z), &[]).unwrap()
        );
    }

    #[test]
    fn editing_the_code_changes_the_execution() {
        let b = RunvaultBlock::default();
        let params = json!({"rows": 13});
        let ex = exclusions(&b, &params);
        let cfg = config_hash(&params, &ex, &[]).unwrap();
        let mut code = Code {
            git_remote: None,
            git_branch: None,
            git_commit: "a".repeat(40),
            git_dirty: false,
            dirty_hash: None,
            repo_relpath: None,
            locks: vec![],
        };
        let before = execution_hash(&cfg, &params, &ex, Some(&code), "env").unwrap();
        code.git_commit = "b".repeat(40);
        let after = execution_hash(&cfg, &params, &ex, Some(&code), "env").unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn two_dirty_working_trees_differ() {
        let b = RunvaultBlock::default();
        let params = json!({});
        let ex = exclusions(&b, &params);
        let cfg = config_hash(&params, &ex, &[]).unwrap();
        let code = |digest: &str| Code {
            git_remote: None,
            git_branch: None,
            git_commit: "a".repeat(40),
            git_dirty: true,
            dirty_hash: Some(Hash::blake3(digest.repeat(64))),
            repo_relpath: None,
            locks: vec![],
        };
        assert_ne!(
            execution_hash(&cfg, &params, &ex, Some(&code("1")), "env").unwrap(),
            execution_hash(&cfg, &params, &ex, Some(&code("2")), "env").unwrap()
        );
    }

    #[test]
    fn the_environment_changes_the_execution_but_not_the_condition() {
        let b = RunvaultBlock::default();
        let params = json!({});
        let ex = exclusions(&b, &params);
        let cfg = config_hash(&params, &ex, &[]).unwrap();
        assert_ne!(
            execution_hash(&cfg, &params, &ex, None, "env-a").unwrap(),
            execution_hash(&cfg, &params, &ex, None, "env-z").unwrap()
        );
    }

    #[test]
    fn env_hash_ignores_the_machine_name_but_not_the_locks() {
        let plain = env_hash("macos", "aarch64", Some("1.94.0"), None, &[]);
        let locked = env_hash(
            "macos",
            "aarch64",
            Some("1.94.0"),
            None,
            &[Lock {
                kind: crate::meta::LockKind::Cargo,
                hash: Hash::blake3("c".repeat(64)),
                file: "lock/Cargo.lock".into(),
            }],
        );
        assert_ne!(plain, locked);
        // Same inputs, same hash: two boxes with the same OS and deps agree.
        assert_eq!(
            plain,
            env_hash("macos", "aarch64", Some("1.94.0"), None, &[])
        );
    }

    #[test]
    fn lock_order_does_not_matter() {
        let cargo = Lock {
            kind: crate::meta::LockKind::Cargo,
            hash: Hash::blake3("1".repeat(64)),
            file: "lock/Cargo.lock".into(),
        };
        let uv = Lock {
            kind: crate::meta::LockKind::Uv,
            hash: Hash::blake3("2".repeat(64)),
            file: "lock/uv.lock".into(),
        };
        assert_eq!(
            env_hash("macos", "aarch64", None, None, &[cargo.clone(), uv.clone()]),
            env_hash("macos", "aarch64", None, None, &[uv, cargo])
        );
    }

    #[test]
    fn a_hashed_dataset_differs_from_an_unhashed_one() {
        let b = RunvaultBlock::default();
        let params = json!({});
        let ex = exclusions(&b, &params);
        let mut d = dataset("train", "a");
        let plain = config_hash(&params, &ex, std::slice::from_ref(&d)).unwrap();
        d.hash = Some(Hash::blake3("f".repeat(64)));
        d.hash_scope = Some(HashScope::File);
        assert_ne!(plain, config_hash(&params, &ex, &[d]).unwrap());
    }

    #[test]
    fn a_manual_run_and_a_code_run_with_an_empty_commit_are_not_confused() {
        // The manual run concatenates three zero-length inputs; a code run cannot
        // reach the same byte string because git_dirty is never empty for it.
        let b = RunvaultBlock::default();
        let params = json!({});
        let ex = exclusions(&b, &params);
        let cfg = config_hash(&params, &ex, &[]).unwrap();
        let code = Code {
            git_remote: None,
            git_branch: None,
            git_commit: String::new(),
            git_dirty: false,
            dirty_hash: None,
            repo_relpath: None,
            locks: vec![],
        };
        assert_ne!(
            execution_hash(&cfg, &params, &ex, None, "env").unwrap(),
            execution_hash(&cfg, &params, &ex, Some(&code), "env").unwrap()
        );
    }
}
