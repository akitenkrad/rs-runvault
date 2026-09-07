//! What a metric means, kept next to the run that recorded it.
//!
//! A metric name alone does not say what was measured. Coming back to an
//! experiment a year later, `mean_n_surviving` is a guess and
//! `normal.q_ordinal.0.mean` is not even that. The design note (§3.11) settles
//! where the answer lives; this module reads it and hands the run its share.
//!
//! ## Why the meaning is not written per name
//!
//! The vault holds **13,589 distinct metric names** (measured 2026-09-07).
//! javitz1991 contributes 11,309 of them and mccanne1993 another 2,219, while
//! every metric a person actually named — across the five social-simulation
//! replications — comes to **74**. The large trees are a product of a few axes:
//!
//! ```text
//! normal.q_ordinal.0.mean
//!    │       │      │   └ aggregation
//!    │       │      └ component index
//!    │       └ measure
//!    └ scenario
//! ```
//!
//! Nobody writes 13,589 descriptions. So a declaration says either what one
//! **name** means, or what the **shape of a family of names** means, one axis at
//! a time. Four lines cover javitz1991's eleven thousand.
//!
//! ## Where it lives, and where it is kept
//!
//! The declaration belongs to the **repository**: what a metric means is a
//! property of the experiment, not of one of its 298 runs. But a run that
//! pointed at a file in the repository would change its meaning whenever that
//! file changed, and lose it whenever the checkout moved. So the declaration is
//! the source and `metrics.meta.json` in the run directory is the record —
//! the same split `config.json` already makes for `parameters`.
//!
//! Only the part that applies is copied. A run that logged three metrics does
//! not carry a description of the other seventy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The file a repository declares its metrics in.
pub const DECLARATION_FILE: &str = "runvault.toml";

/// The file written into the run directory.
pub const META_FILE: &str = "metrics.meta.json";

/// Which way is better.
///
/// Three values and no more. A screen that knows this can say which of two runs
/// improved; one that does not must stay silent rather than guess, which is why
/// `None` is a value a person can write rather than the absence of the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Higher is better.
    Up,
    /// Lower is better.
    Down,
    /// Neither — the number is a description, not a score.
    None,
}

/// What one metric measures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDoc {
    /// What is being measured. The one field that is required.
    pub meaning: String,
    /// The unit the number is in: `ratio`, `count`, `sec`, `usd`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Which way is better, when that can be said at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// The values it can take, if bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// The aggregation levels it is meaningful at, as in the core vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
}

/// What a family of names means, one axis at a time.
///
/// `pattern` is a dotted template. A segment written `{name}` is an axis and
/// matches any one segment; every other segment must match exactly. The axes
/// are described in `axes`, keyed by the name inside the braces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricPattern {
    /// The dotted template, with axes written `{name}`.
    pub pattern: String,
    /// What the whole family measures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meaning: Option<String>,
    /// The unit every member of the family is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Which way is better, for every member of the family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Axis name (without braces) to what that position means.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub axes: BTreeMap<String, String>,
}

impl MetricPattern {
    /// Whether `name` is one of the names this pattern describes.
    ///
    /// Segment counts must agree: `a.{x}` does not describe `a.b.c`. Matching on
    /// a prefix instead would let one pattern quietly claim a deeper family that
    /// nobody meant to describe.
    pub fn matches(&self, name: &str) -> bool {
        let want: Vec<&str> = self.pattern.split('.').collect();
        let got: Vec<&str> = name.split('.').collect();
        if want.len() != got.len() {
            return false;
        }
        want.iter()
            .zip(got.iter())
            .all(|(w, g)| is_axis(w) || w == g)
    }

    /// The axis names, in the order they appear in the pattern.
    pub fn axis_order(&self) -> Vec<String> {
        self.pattern
            .split('.')
            .filter(|s| is_axis(s))
            .map(|s| s[1..s.len() - 1].to_string())
            .collect()
    }
}

fn is_axis(segment: &str) -> bool {
    segment.len() > 2 && segment.starts_with('{') && segment.ends_with('}')
}

/// A repository's declaration, as read from `runvault.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Declaration {
    /// What one named metric measures.
    #[serde(default)]
    pub metrics: BTreeMap<String, MetricDoc>,
    /// What a family of names measures.
    #[serde(default)]
    pub metric_patterns: Vec<MetricPattern>,
}

/// What is written into the run directory.
///
/// `unmatched` is the part of the answer that is missing: names this run
/// recorded that nothing describes. It is written out rather than left implicit
/// so a reader can tell "nobody wrote this down" from "the file is incomplete".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsMeta {
    /// The shape of this file.
    pub schema_version: String,
    /// The named metrics this run recorded, and what they mean.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, MetricDoc>,
    /// The families that describe the rest of this run's names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<MatchedPattern>,
    /// Names this run recorded that nothing describes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undescribed: Vec<String>,
}

/// A pattern that described at least one of this run's names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchedPattern {
    /// The family, as the repository declared it.
    #[serde(flatten)]
    pub pattern: MetricPattern,
    /// How many of this run's names it describes.
    pub n_matched: usize,
}

impl Declaration {
    /// Reads `<repo_root>/runvault.toml`.
    ///
    /// A repository without one is not an error: every replication that exists
    /// today has no declaration, and refusing to run there would make the
    /// feature a breaking change rather than an addition.
    pub fn load(repo_root: &Path) -> Result<Option<Self>> {
        let path: PathBuf = repo_root.join(DECLARATION_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(Error::PlainIo)?;
        let parsed: Self =
            toml::from_str(&text).map_err(|e| Error::Spec(format!("{}: {e}", path.display())))?;
        Ok(Some(parsed))
    }

    /// The share of the declaration that applies to `names`.
    ///
    /// A run carries what it measured and nothing else — copying the whole
    /// declaration into every run would repeat javitz1991's four patterns 298
    /// times, and would claim the run recorded metrics it never touched.
    pub fn resolve(&self, names: &BTreeSet<String>) -> MetricsMeta {
        let mut metrics = BTreeMap::new();
        let mut counts: Vec<usize> = vec![0; self.metric_patterns.len()];
        let mut undescribed = Vec::new();

        for name in names {
            if let Some(doc) = self.metrics.get(name) {
                metrics.insert(name.clone(), doc.clone());
                continue;
            }
            // The exact name wins over a pattern: a family can be described in
            // general and one of its members called out in particular.
            match self.metric_patterns.iter().position(|p| p.matches(name)) {
                Some(i) => counts[i] += 1,
                None => undescribed.push(name.clone()),
            }
        }

        let patterns = self
            .metric_patterns
            .iter()
            .zip(counts)
            .filter(|(_, n)| *n > 0)
            .map(|(p, n)| MatchedPattern {
                pattern: p.clone(),
                n_matched: n,
            })
            .collect();

        MetricsMeta {
            schema_version: "1.0".into(),
            metrics,
            patterns,
            undescribed,
        }
    }
}

impl MetricsMeta {
    /// Whether there is anything worth writing.
    ///
    /// A run whose every name is undescribed writes nothing: a file that says
    /// only "nothing is documented" is the same claim as the absent file, and
    /// putting it in every run directory would add a file to 1,222 records to
    /// say so.
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty() && self.patterns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn javitz() -> Declaration {
        toml::from_str(
            r#"
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
            "#,
        )
        .unwrap()
    }

    #[test]
    fn a_pattern_describes_a_family_of_names() {
        let d = javitz();
        let p = &d.metric_patterns[0];
        assert!(p.matches("normal.q_ordinal.0.mean"));
        assert!(p.matches("masquerade.q_ordinal.17.max"));
        assert!(
            !p.matches("normal.other.0.mean"),
            "固定の語まで当ててはいけない"
        );
    }

    #[test]
    fn a_pattern_does_not_reach_into_a_deeper_name() {
        // 前方一致にすると，誰も説明していない深い一族を勝手に取り込む．
        let d = javitz();
        assert!(!d.metric_patterns[0].matches("normal.q_ordinal.0.mean.extra"));
        assert!(!d.metric_patterns[0].matches("normal.q_ordinal.0"));
    }

    #[test]
    fn the_axes_are_read_in_the_order_they_appear() {
        assert_eq!(
            javitz().metric_patterns[0].axis_order(),
            ["scenario", "index", "agg"]
        );
    }

    #[test]
    fn a_run_carries_only_what_it_measured() {
        let meta = javitz().resolve(&names(&["normal.q_ordinal.0.mean"]));
        // 使っていない `convergence_rate` の説明は写らない．
        assert!(meta.metrics.is_empty(), "{meta:?}");
        assert_eq!(meta.patterns.len(), 1);
        assert_eq!(meta.patterns[0].n_matched, 1);
    }

    #[test]
    fn a_named_metric_beats_the_pattern_that_would_also_match() {
        let d: Declaration = toml::from_str(
            r#"
            [metrics."normal.q_ordinal.0.mean"]
            meaning = "この 1 つだけは別の意味を持つ"

            [[metric_patterns]]
            pattern = "{scenario}.q_ordinal.{index}.{agg}"
            "#,
        )
        .unwrap();
        let meta = d.resolve(&names(&[
            "normal.q_ordinal.0.mean",
            "normal.q_ordinal.1.mean",
        ]));
        assert_eq!(meta.metrics.len(), 1);
        assert_eq!(meta.patterns[0].n_matched, 1);
    }

    #[test]
    fn a_name_nothing_describes_is_named_rather_than_dropped() {
        // 黙って落とすと «説明が無い» ことに気づけない．
        let meta = javitz().resolve(&names(&["nobody_wrote_this_down"]));
        assert_eq!(meta.undescribed, ["nobody_wrote_this_down"]);
        assert!(meta.is_empty(), "説明が 1 つも無いなら書くものは無い");
    }

    #[test]
    fn a_declaration_is_optional() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Declaration::load(dir.path()).unwrap(), None);
    }

    #[test]
    fn a_broken_declaration_says_which_file_it_is() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DECLARATION_FILE), "[metrics\n").unwrap();
        let err = Declaration::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains(DECLARATION_FILE), "{err}");
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::meta::Origin;
    use crate::run::{Run, RunOptions};

    fn repo_with(declaration: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(text) = declaration {
            std::fs::write(dir.path().join(DECLARATION_FILE), text).unwrap();
        }
        dir
    }

    fn run_in(repo: &Path, results: &Path) -> Run {
        Run::start(
            RunOptions::new("e", "main")
                .repo_id("demo")
                .domain("other")
                .origin(Origin::Manual)
                .results_root(results)
                .repo_root(repo)
                .parameters(&serde_json::json!({}))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_run_writes_what_its_metrics_mean() {
        let repo = repo_with(Some(
            r#"
            [metrics."converged"]
            meaning = "収束したか"
            direction = "up"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("converged", 1.0).send().unwrap();
        let dir = run.finish().unwrap();

        let meta: MetricsMeta = crate::files::read_json(&dir.join(META_FILE)).unwrap();
        assert_eq!(meta.metrics["converged"].meaning, "収束したか");
        assert_eq!(meta.metrics["converged"].direction, Some(Direction::Up));
    }

    #[test]
    fn a_repository_without_a_declaration_still_runs() {
        // 既存の 5 リポジトリはどれも宣言を持たない．足した機能で走らなくなっては困る．
        let repo = repo_with(None);
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("converged", 1.0).send().unwrap();
        let dir = run.finish().unwrap();
        assert!(!dir.join(META_FILE).exists());
    }

    #[test]
    fn a_run_does_not_carry_descriptions_of_metrics_it_never_logged() {
        let repo = repo_with(Some(
            r#"
            [metrics."used"]
            meaning = "この run が測ったもの"

            [metrics."unused"]
            meaning = "別の run が測るもの"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("used", 1.0).send().unwrap();
        let dir = run.finish().unwrap();

        let meta: MetricsMeta = crate::files::read_json(&dir.join(META_FILE)).unwrap();
        assert_eq!(meta.metrics.len(), 1, "{meta:?}");
        assert!(meta.metrics.contains_key("used"));
    }

    #[test]
    fn nothing_is_written_when_nothing_is_described() {
        // «何も説明されていない» と書いたファイルは，無いファイルと同じことしか
        // 言っていない．1,222 の run に置く理由がない．
        let repo = repo_with(Some(
            r#"
            [metrics."something_else"]
            meaning = "この run は測っていない"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("undocumented_metric", 1.0).send().unwrap();
        let dir = run.finish().unwrap();
        assert!(!dir.join(META_FILE).exists());
    }
}
