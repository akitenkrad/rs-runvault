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

/// The file the condition's descriptions are written into.
///
/// A second file rather than a section of the first: the two are independent —
/// a repository may describe its metrics and not its conditions — and each is
/// written only when something applies. One file carrying "nothing here" for
/// the half that was not declared says less than its absence does.
pub const PARAMETERS_META_FILE: &str = "parameters.meta.json";

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

/// What one setting of the experimental condition is.
///
/// No `direction`: a condition is not a score, so "which way is better" has
/// nothing to say about it. What a run *reached* is a metric; what it was
/// *asked to do* is this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParameterDoc {
    /// What the setting controls. The one field that is required.
    pub meaning: String,
    /// The unit the value is in, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The values it is meant to take, if bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
}

/// A repository's declaration, as read from `runvault.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Declaration {
    /// Whether every metric this repository records has to be described.
    ///
    /// True unless the file says otherwise. A description is not worth much if
    /// it is written for the metrics somebody remembered and skipped for the
    /// rest: the ones that go undescribed are exactly the ones nobody could
    /// name a year later. A repository that genuinely does not want to describe
    /// its metrics says so here, in one line, rather than by omission — so that
    /// "not described" and "chose not to describe" stay apart.
    #[serde(default = "yes")]
    pub require_docs: bool,
    /// What one named metric measures.
    #[serde(default)]
    pub metrics: BTreeMap<String, MetricDoc>,
    /// What a family of names measures.
    #[serde(default)]
    pub metric_patterns: Vec<MetricPattern>,
    /// What one setting of the condition is, keyed by JSON pointer.
    ///
    /// Pointers rather than bare names, because `hash_exclude` and
    /// `seed_pointers` already speak in pointers and one file should not hold
    /// two spellings of "which setting". They also reach a nested condition,
    /// which a bare name cannot.
    #[serde(default)]
    pub parameters: BTreeMap<String, ParameterDoc>,
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

/// What is written into `parameters.meta.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParametersMeta {
    /// The shape of this file.
    pub schema_version: String,
    /// The settings this run actually carries, and what they are.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, ParameterDoc>,
    /// Pointers this run carries that nothing describes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undescribed: Vec<String>,
}

impl ParametersMeta {
    /// Whether there is anything worth writing.
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }
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

/// `require_docs`'s default. Serde wants a function, not a literal.
fn yes() -> bool {
    true
}

impl Default for Declaration {
    fn default() -> Self {
        Self {
            require_docs: true,
            metrics: BTreeMap::new(),
            metric_patterns: Vec::new(),
            parameters: BTreeMap::new(),
        }
    }
}

impl Declaration {
    /// Reads `<repo_root>/runvault.toml`.
    ///
    /// `None` means the file is not there. That is no longer the same as "this
    /// repository has nothing to declare": since descriptions became required,
    /// a repository under a known root that records a metric without this file
    /// is refused at the moment it records one (see `Run`). The distinction is
    /// kept here rather than resolved, because the caller is the one that knows
    /// whether a root was known at all.
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

    /// Whether this declaration says what `name` measures.
    ///
    /// The exact name first, a family second — the same order `resolve` and the
    /// audit use, so that what is allowed to be recorded and what is counted as
    /// described can never disagree.
    ///
    /// The reserved names of the core vocabulary are *not* asked about here.
    /// Their meaning is the registry's, not a repository's, and the caller adds
    /// them.
    pub fn describes(&self, name: &str) -> bool {
        self.metrics.contains_key(name) || self.metric_patterns.iter().any(|p| p.matches(name))
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

        let vocabulary = crate::vocabulary::get();
        for name in names {
            if let Some(doc) = self.metrics.get(name) {
                metrics.insert(name.clone(), doc.clone());
                continue;
            }
            // A reserved name is described by the registry, once, for every
            // repository. Listing it here would have the run claim nobody said
            // what `n_units` is, next to the vocabulary that says it.
            if vocabulary.metric_names.contains_key(name.as_str()) {
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

impl Declaration {
    /// The share of the parameter declaration this run's condition carries.
    ///
    /// A pointer that does not resolve in `parameters` describes a setting this
    /// run does not have, and is left out — the same rule the metric side
    /// follows. A setting the run has and nobody described is listed in
    /// `undescribed`, so "nobody wrote this down" stays visible.
    pub fn resolve_parameters(&self, parameters: &serde_json::Value) -> ParametersMeta {
        let mut described = BTreeMap::new();
        for (raw, doc) in &self.parameters {
            let Ok(pointer) = crate::pointer::Pointer::parse(raw) else {
                // A pointer that does not parse describes nothing. It is caught
                // when the declaration is read, not silently applied here.
                continue;
            };
            if pointer.resolve(parameters).is_some() {
                described.insert(raw.clone(), doc.clone());
            }
        }

        // Only the top level is counted as undescribed. A nested setting can be
        // described by pointing at it or at the object above it, and calling
        // every leaf of a deep condition "undescribed" would report a hole
        // where the parent already says what the whole block is.
        let mut undescribed = Vec::new();
        if let serde_json::Value::Object(map) = parameters {
            for key in map.keys() {
                let pointer = format!("/{}", key.replace('~', "~0").replace('/', "~1"));
                if !described.contains_key(&pointer) {
                    undescribed.push(pointer);
                }
            }
        }

        ParametersMeta {
            schema_version: "1.0".into(),
            parameters: described,
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
    fn a_repository_without_a_declaration_cannot_record_a_metric() {
        // 説明は必須になった（2026-09-08）．宣言ファイルが無いリポジトリは，
        // «説明することが無い» のではなく «まだ書いていない» のだから，拒む．
        let repo = repo_with(None);
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        let err = run.log_metric("converged", 1.0).send().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("converged"), "{message}");
        assert!(message.contains(DECLARATION_FILE), "{message}");
        assert!(message.contains("require_docs"), "{message}");
    }

    #[test]
    fn a_repository_may_say_it_does_not_describe_its_metrics() {
        // 逃げ道は 1 行で，明示的に書く．書かなかったこと（＝ファイルの不在）と
        // 書かないと決めたことを分けておく．
        let repo = repo_with(Some("require_docs = false\n"));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("converged", 1.0).send().unwrap();
        let dir = run.finish().unwrap();
        assert!(!dir.join(META_FILE).exists());
    }

    #[test]
    fn an_undescribed_metric_is_refused_where_it_is_recorded() {
        // 落ちるのは記録した瞬間．sweep が 15 分かけて計算し終えてからでは，
        // 直して走らせ直す費用が «書き忘れた 1 行» に見合わない．
        let repo = repo_with(Some(
            r#"
            [metrics."described"]
            meaning = "宣言のある指標"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("described", 1.0).send().unwrap();
        let err = run.log_metric("forgotten", 2.0).send().unwrap_err();
        assert!(err.to_string().contains("forgotten"), "{err}");

        // 拒まれた行は書かれていない — 記録の前に問うているため．
        let text = std::fs::read_to_string(run.dir().join("metrics.csv")).unwrap();
        assert!(!text.contains("forgotten"), "{text}");
    }

    #[test]
    fn a_family_is_enough_to_let_a_name_through() {
        // javitz1991 の 11,309 件を名前ごとに書くことはできない．形で通す．
        let repo = repo_with(Some(
            r#"
            [[metric_patterns]]
            pattern = "{scenario}.q_ordinal.{measure}.{statistic}"
            meaning = "Q 統計量"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("normal.q_ordinal.0.mean", 1.0).send().unwrap();
        // 区画の数が合わない名前には当たらないので，これは通らない．
        let err = run.log_metric("normal.q_ordinal.0", 1.0).send().unwrap_err();
        assert!(err.to_string().contains("normal.q_ordinal.0"), "{err}");
    }

    #[test]
    fn a_reserved_name_needs_no_declaration() {
        // 予約指標の意味は語彙が決めている．リポジトリに書き写させない．
        let repo = repo_with(Some(
            r#"
            [metrics."described"]
            meaning = "宣言のある指標"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = run_in(repo.path(), results.path());
        run.log_metric("n_units", 42.0).send().unwrap();
        let dir = run.finish().unwrap();
        // 語彙が説明しているので «説明が無い» とは書かない．
        assert!(!dir.join(META_FILE).exists());
    }

    #[test]
    fn a_run_raised_outside_any_repository_is_not_asked() {
        // 宣言ファイルの置き場所が分からない run に，その置き場所を要求できない．
        let results = tempfile::tempdir().unwrap();
        let mut run = Run::start(
            RunOptions::new("e", "main")
                .repo_id("demo")
                .domain("other")
                .origin(Origin::Manual)
                .results_root(results.path())
                .parameters(&serde_json::json!({}))
                .unwrap(),
        )
        .unwrap();
        run.log_metric("whatever", 1.0).send().unwrap();
        run.finish().unwrap();
    }

    #[test]
    fn a_malformed_declaration_is_an_error_not_an_absence() {
        // 握り潰すと，打ち間違えた runvault.toml が «何も宣言していない» に化け，
        // 最初の指標で «ファイルが無い» と言われる（見当違いの案内になる）．
        let repo = repo_with(Some("[metrics.\"x\"]\nmeaning = \n"));
        let results = tempfile::tempdir().unwrap();
        let started = Run::start(
            RunOptions::new("e", "main")
                .repo_id("demo")
                .domain("other")
                .origin(Origin::Manual)
                .results_root(results.path())
                .repo_root(repo.path())
                .parameters(&serde_json::json!({}))
                .unwrap(),
        );
        let Err(err) = started else {
            panic!("打ち間違えた宣言で run が始まってしまった");
        };
        assert!(err.to_string().contains(DECLARATION_FILE), "{err}");
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
        // 言っていない．説明が必須になった今も，説明を要さない予約指標だけを
        // 記録した run はこの形になる．
        let repo = repo_with(Some(
            r#"
            require_docs = false

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

    // -----------------------------------------------------------------
    // 条件（parameters）の説明（MYTASK-3235）
    // -----------------------------------------------------------------

    fn with_parameters() -> Declaration {
        toml::from_str(
            r#"
            [parameters."/eps_l"]
            meaning = "左側の信頼幅"
            range = [0.0, 1.0]

            [parameters."/n"]
            meaning = "エージェント数"
            unit = "count"

            [parameters."/grid/start"]
            meaning = "走査の始点"

            [parameters."/never_used"]
            meaning = "この run には無い設定"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn a_run_carries_only_the_settings_it_has() {
        let params = serde_json::json!({"eps_l": 0.01, "n": 625, "grid": {"start": 0.0}});
        let meta = with_parameters().resolve_parameters(&params);
        assert_eq!(meta.parameters.len(), 3, "{meta:?}");
        assert_eq!(meta.parameters["/eps_l"].meaning, "左側の信頼幅");
        assert_eq!(meta.parameters["/n"].unit.as_deref(), Some("count"));
        // 入れ子もポインタで指せる．
        assert!(meta.parameters.contains_key("/grid/start"));
        // この run が持たない設定の説明は写らない．
        assert!(!meta.parameters.contains_key("/never_used"));
    }

    #[test]
    fn a_setting_nobody_described_is_listed_rather_than_dropped() {
        let params = serde_json::json!({"eps_l": 0.01, "tol": 1e-6});
        let meta = with_parameters().resolve_parameters(&params);
        assert_eq!(meta.undescribed, ["/tol"], "{meta:?}");
    }

    #[test]
    fn a_described_parent_covers_the_settings_under_it() {
        // 入れ子の葉を全部 «説明なし» と数えると，親が «この塊は何か» を
        // 言っているのに穴があるように見える．数えるのは最上位だけ．
        let d: Declaration = toml::from_str(
            r#"
            [parameters."/grid"]
            meaning = "走査するグリッドの定義"
            "#,
        )
        .unwrap();
        let params = serde_json::json!({"grid": {"start": 0.0, "stop": 1.0, "step": 0.1}});
        let meta = d.resolve_parameters(&params);
        assert!(meta.undescribed.is_empty(), "{meta:?}");
    }

    #[test]
    fn finish_writes_the_conditions_the_run_actually_had() {
        let repo = repo_with(Some(
            r#"
            [parameters."/eps"]
            meaning = "信頼幅 ε"
            range = [0.0, 1.0]

            [parameters."/unused"]
            meaning = "この run には無い"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let run = Run::start(
            RunOptions::new("e", "main")
                .repo_id("demo")
                .domain("other")
                .origin(Origin::Manual)
                .results_root(results.path())
                .repo_root(repo.path())
                .parameters(&serde_json::json!({"eps": 0.15, "tol": 1e-6}))
                .unwrap(),
        )
        .unwrap();
        let dir = run.finish().unwrap();

        let meta: ParametersMeta =
            crate::files::read_json(&dir.join(PARAMETERS_META_FILE)).unwrap();
        assert_eq!(meta.parameters["/eps"].meaning, "信頼幅 ε");
        assert!(!meta.parameters.contains_key("/unused"));
        assert_eq!(meta.undescribed, ["/tol"]);
    }

    #[test]
    fn no_file_when_the_repository_described_no_condition() {
        // 指標だけ宣言したリポジトリで，条件のファイルが増えないこと．
        let repo = repo_with(Some(
            r#"
            [metrics."converged"]
            meaning = "収束したか"
            "#,
        ));
        let results = tempfile::tempdir().unwrap();
        let mut run = Run::start(
            RunOptions::new("e", "main")
                .repo_id("demo")
                .domain("other")
                .origin(Origin::Manual)
                .results_root(results.path())
                .repo_root(repo.path())
                .parameters(&serde_json::json!({"eps": 0.15}))
                .unwrap(),
        )
        .unwrap();
        run.log_metric("converged", 1.0).send().unwrap();
        let dir = run.finish().unwrap();
        assert!(dir.join(META_FILE).exists(), "指標の側は書かれる");
        assert!(!dir.join(PARAMETERS_META_FILE).exists());
    }

    #[test]
    fn nothing_is_written_when_no_setting_is_described() {
        let d: Declaration = toml::from_str(
            r#"
            [parameters."/something_else"]
            meaning = "この run は持っていない"
            "#,
        )
        .unwrap();
        let meta = d.resolve_parameters(&serde_json::json!({"eps_l": 0.01}));
        assert!(meta.is_empty(), "{meta:?}");
        // 説明が 1 つも無くても «説明されていない設定» は数える．
        assert_eq!(meta.undescribed, ["/eps_l"]);
    }
}
