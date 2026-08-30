//! `config.json` — the envelope around the experimental condition.
//!
//! Only `/parameters` is hashed. `run_uid` and the control block sit outside it,
//! so the hash neither changes per run nor contains itself (design note §3.3).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::pointer::Pointer;

/// Keys the experiment declares as not affecting the result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Determinism {
    /// Pointers whose value the experiment guarantees does not change the result.
    /// Conditional exclusion is opt-in: excluding `/threads` unconditionally would
    /// bundle runs with genuinely different results as one condition.
    #[serde(default)]
    pub invariant_to: Vec<String>,
}

/// The control block: what to exclude from hashes and what to sync.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RunvaultBlock {
    /// Pointers removed from every hash.
    #[serde(default)]
    pub hash_exclude: Vec<String>,
    /// Where the seeds are. Removed from `config_hash`, kept in `execution_hash`.
    #[serde(default)]
    pub seed_pointers: Vec<String>,
    /// Conditional exclusions.
    #[serde(default)]
    pub determinism: Determinism,
    /// Globs added to the sync set.
    #[serde(default)]
    pub sync_include: Vec<String>,
    /// Globs kept out of the sync set. Wins over `sync_include`.
    #[serde(default)]
    pub sync_exclude: Vec<String>,
}

/// `config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ConfigEnvelope {
    /// Version of `schema/v1`.
    pub schema_version: String,
    /// The run this configuration belongs to.
    pub run_uid: String,
    /// The control block.
    pub runvault: RunvaultBlock,
    /// The experimental condition. The only part that is hashed.
    pub parameters: Value,
}

/// The three pointer lists, resolved and with the precedence of §3.3 applied.
#[derive(Debug, Clone, Default)]
pub struct Exclusions {
    /// Removed from every hash.
    pub hash_exclude: Vec<Pointer>,
    /// Removed from `config_hash`, fed into `execution_hash`.
    pub seeds: Vec<Pointer>,
    /// Removed from every hash, but only because the experiment declared it.
    pub invariant_to: Vec<Pointer>,
}

impl Exclusions {
    /// Parses the pointer lists and applies precedence: `hash_exclude` beats
    /// `seed_pointers` beats `invariant_to`. Every pointer must resolve inside
    /// `parameters`, so an exclusion cannot silently fail to apply.
    pub fn resolve(block: &RunvaultBlock, parameters: &Value) -> Result<Self> {
        let parse = |raws: &[String]| -> Result<Vec<Pointer>> {
            let mut out = Vec::new();
            for raw in raws {
                let pointer = Pointer::parse(raw)?;
                pointer.require(parameters)?;
                if !out.contains(&pointer) {
                    out.push(pointer);
                }
            }
            Ok(out)
        };

        let hash_exclude = parse(&block.hash_exclude)?;
        let seeds: Vec<Pointer> = parse(&block.seed_pointers)?
            .into_iter()
            .filter(|p| !hash_exclude.contains(p))
            .collect();
        let invariant_to: Vec<Pointer> = parse(&block.determinism.invariant_to)?
            .into_iter()
            .filter(|p| !hash_exclude.contains(p) && !seeds.contains(p))
            .collect();

        Ok(Self { hash_exclude, seeds, invariant_to })
    }

    /// Everything removed before `config_hash` is taken.
    pub fn removed_from_config(&self) -> Vec<Pointer> {
        let mut out = self.hash_exclude.clone();
        out.extend(self.seeds.iter().cloned());
        out.extend(self.invariant_to.iter().cloned());
        out
    }

    /// The seeds, ordered by pointer string as `execution_hash` requires.
    pub fn ordered_seeds(&self) -> Vec<Pointer> {
        let mut out = self.seeds.clone();
        out.sort_by(|a, b| a.raw().cmp(b.raw()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn hash_exclude_beats_seed_pointers() {
        let params = json!({"seed": 1, "threads": 8});
        let ex = Exclusions::resolve(&block(&["/seed"], &["/seed"], &[]), &params).unwrap();
        assert!(ex.seeds.is_empty());
        assert_eq!(ex.hash_exclude.len(), 1);
    }

    #[test]
    fn seed_pointers_beat_invariant_to() {
        let params = json!({"seed": 1});
        let ex = Exclusions::resolve(&block(&[], &["/seed"], &["/seed"]), &params).unwrap();
        assert_eq!(ex.seeds.len(), 1);
        assert!(ex.invariant_to.is_empty());
    }

    #[test]
    fn a_pointer_that_does_not_resolve_is_an_error() {
        let params = json!({"seed": 1});
        assert!(Exclusions::resolve(&block(&["/nope"], &[], &[]), &params).is_err());
    }

    #[test]
    fn seeds_are_ordered_by_pointer_string() {
        let params = json!({"b": 1, "a": 2});
        let ex = Exclusions::resolve(&block(&[], &["/b", "/a"], &[]), &params).unwrap();
        let seeds = ex.ordered_seeds();
        let ordered: Vec<&str> = seeds.iter().map(|p| p.raw()).collect();
        assert_eq!(ordered, ["/a", "/b"]);
    }

    #[test]
    fn duplicate_pointers_are_collapsed() {
        let params = json!({"a": 1});
        let ex = Exclusions::resolve(&block(&["/a", "/a"], &[], &[]), &params).unwrap();
        assert_eq!(ex.hash_exclude.len(), 1);
    }
}
