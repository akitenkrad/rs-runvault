//! `runvault metrics audit` — how much of the vault says what it measured.
//!
//! The declaration is optional and will stay optional: the 1,222 runs that
//! already exist have no `runvault.toml` behind them, and no description can be
//! added to them after the fact (design note §3.11). Optional without a count is
//! the same as never — so this command counts, and decides nothing. It removes
//! nothing, refuses nothing, and is deliberately not part of `verify`.
//!
//! ## Why the answer is folded
//!
//! javitz1991 records 11,309 distinct metric names. Printing them is not a
//! report, it is a wall. Names described by a declared family are counted as
//! that family — one line, `N` names — which is the same move the declaration
//! itself makes: four patterns for eleven thousand names. What is left over,
//! the part nobody described, is printed by name, because a count alone cannot
//! be acted on; when there are too many of those the list is cut and says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use duckdb::Connection;
use runvault::metrics_meta::MetricPattern;
use serde_json::{Value, json};

use crate::index::INDEX_DIR;
use crate::report::{each_row, table, text};

/// How many undescribed names are printed per experiment unless told otherwise.
pub const DEFAULT_LIMIT: usize = 20;

/// Where the description of one metric name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Described {
    /// The repository declared this exact name.
    ByName,
    /// The core vocabulary reserves the name and fixes its meaning.
    ByVocabulary,
    /// A declared family covers it, by the index of that family.
    ByFamily(usize),
    /// Nobody wrote it down.
    Not,
}

/// A declared family, and how many of an experiment's names it covers.
#[derive(Debug, Clone)]
pub struct Family {
    /// The dotted template, axes written `{name}`.
    pub pattern: String,
    /// What the family measures, if the declaration said.
    pub meaning: Option<String>,
    /// How many of this experiment's names it describes.
    pub n_matched: usize,
}

/// The count for one experiment.
#[derive(Debug, Clone)]
pub struct ExperimentAudit {
    /// The repository the runs came from.
    pub repo_id: Option<String>,
    /// The experiment, or nothing for a legacy run that sat at the root.
    pub experiment: Option<String>,
    /// Distinct metric names the experiment's runs recorded.
    pub n_names: usize,
    /// Names the repository declared one by one.
    pub n_by_name: usize,
    /// Names whose meaning the core vocabulary fixes.
    pub n_by_vocabulary: usize,
    /// The declared families that cover at least one name.
    pub families: Vec<Family>,
    /// Every name nothing describes, in order.
    pub undescribed: Vec<String>,
}

impl ExperimentAudit {
    /// Names something describes.
    pub fn n_described(&self) -> usize {
        self.n_names - self.undescribed.len()
    }
}

/// The whole count, one entry per experiment.
#[derive(Debug, Clone, Default)]
pub struct Audit {
    /// Every experiment that recorded a metric, by repository then name.
    pub experiments: Vec<ExperimentAudit>,
}

impl Audit {
    /// Names something describes, added up across experiments.
    pub fn n_described(&self) -> usize {
        self.experiments.iter().map(|e| e.n_described()).sum()
    }

    /// Names nothing describes, added up across experiments.
    pub fn n_undescribed(&self) -> usize {
        self.experiments.iter().map(|e| e.undescribed.len()).sum()
    }
}

/// One experiment, while it is being assembled.
#[derive(Default)]
struct Bucket {
    names: BTreeSet<String>,
    docs: BTreeSet<String>,
    patterns: Vec<(String, Option<String>)>,
}

type Key = (Option<String>, Option<String>);

/// Counts the vault's index.
pub fn build(vault_root: &Path) -> Result<Audit, String> {
    let index = vault_root.join(INDEX_DIR);
    for name in ["runs", "metrics", "metric_docs"] {
        let path = index.join(format!("{name}.parquet"));
        if !path.exists() {
            return Err(format!(
                "{} がありません. 先に `runvault query --refresh` で索引を作ってください",
                path.display()
            ));
        }
    }
    let connection = Connection::open_in_memory().map_err(|e| e.to_string())?;
    let runs = table(vault_root, "runs");
    let metrics = table(vault_root, "metrics");
    let docs = table(vault_root, "metric_docs");

    let mut buckets: BTreeMap<Key, Bucket> = BTreeMap::new();

    // The names an experiment actually recorded. `reference.csv` is not asked:
    // a reported value is the paper's number, and the question here is what
    // this vault measured.
    each_row(
        &connection,
        &format!(
            "SELECT DISTINCT r.repo_id, r.experiment, m.name \
             FROM {metrics} m JOIN {runs} r USING (run_key) \
             ORDER BY 1, 2, 3"
        ),
        |row| {
            let Some(name) = text(&row[2]) else {
                return Ok(());
            };
            buckets
                .entry((text(&row[0]), text(&row[1])))
                .or_default()
                .names
                .insert(name);
            Ok(())
        },
    )?;

    // The descriptions the runs carry. They are per run, and the same
    // declaration reaches every run of the experiment, so they are gathered
    // per experiment and deduplicated.
    each_row(
        &connection,
        &format!(
            "SELECT DISTINCT r.repo_id, r.experiment, d.kind, d.key, d.meaning \
             FROM {docs} d JOIN {runs} r USING (run_key) \
             ORDER BY 1, 2, 3, 4"
        ),
        |row| {
            let (Some(kind), Some(key)) = (text(&row[2]), text(&row[3])) else {
                return Ok(());
            };
            let bucket = buckets.entry((text(&row[0]), text(&row[1]))).or_default();
            match kind.as_str() {
                "name" => {
                    bucket.docs.insert(key);
                }
                "pattern" => bucket.patterns.push((key, text(&row[4]))),
                _ => {}
            }
            Ok(())
        },
    )?;

    let vocabulary = runvault::vocabulary::get();
    let mut experiments = Vec::new();
    for ((repo_id, experiment), bucket) in buckets {
        let patterns: Vec<MetricPattern> = bucket
            .patterns
            .iter()
            .map(|(pattern, meaning)| MetricPattern {
                pattern: pattern.clone(),
                meaning: meaning.clone(),
                unit: None,
                direction: None,
                axes: BTreeMap::new(),
            })
            .collect();

        let mut n_by_name = 0;
        let mut n_by_vocabulary = 0;
        let mut per_family = vec![0usize; patterns.len()];
        let mut undescribed = Vec::new();
        for name in &bucket.names {
            match classify(name, &bucket.docs, vocabulary, &patterns) {
                Described::ByName => n_by_name += 1,
                Described::ByVocabulary => n_by_vocabulary += 1,
                Described::ByFamily(i) => per_family[i] += 1,
                Described::Not => undescribed.push(name.clone()),
            }
        }

        let families = bucket
            .patterns
            .into_iter()
            .zip(per_family)
            .filter(|(_, n)| *n > 0)
            .map(|((pattern, meaning), n_matched)| Family {
                pattern,
                meaning,
                n_matched,
            })
            .collect();

        experiments.push(ExperimentAudit {
            repo_id,
            experiment,
            n_names: bucket.names.len(),
            n_by_name,
            n_by_vocabulary,
            families,
            undescribed,
        });
    }
    Ok(Audit { experiments })
}

/// Where one name's description comes from, if it has one.
///
/// The order is the order of authority. A repository that declared the exact
/// name meant that one; the registry's reserved names are not a repository's to
/// redefine, and are asked before the families so a one-segment pattern cannot
/// quietly claim `n_units`; a family is what is left.
fn classify(
    name: &str,
    docs: &BTreeSet<String>,
    vocabulary: &runvault::vocabulary::Vocabulary,
    patterns: &[MetricPattern],
) -> Described {
    if docs.contains(name) {
        return Described::ByName;
    }
    if vocabulary.metric_names.contains_key(name) {
        return Described::ByVocabulary;
    }
    match patterns.iter().position(|p| p.matches(name)) {
        Some(i) => Described::ByFamily(i),
        None => Described::Not,
    }
}

/// A count with the digits grouped, so five figures can be read at a glance.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How one experiment is named on the page.
fn heading(entry: &ExperimentAudit) -> String {
    let repo = entry.repo_id.as_deref().unwrap_or("(リポジトリ不明)");
    match entry.experiment.as_deref() {
        Some(experiment) => format!("{repo} / {experiment}"),
        // A legacy run that sat directly under `results/` records no
        // experiment, and a plausible one is not invented for it.
        None => format!("{repo} / (実験名なし)"),
    }
}

/// The names to print, and whether the list was cut.
///
/// `limit` of zero prints everything, which is what a caller reading the JSON
/// wants: the cut exists so a person can read the output, not to hide names.
fn shown(names: &[String], limit: usize) -> (&[String], bool) {
    if limit == 0 || names.len() <= limit {
        (names, false)
    } else {
        (&names[..limit], true)
    }
}

/// The report a person reads.
pub fn render(audit: &Audit, limit: usize) -> String {
    let mut out = String::new();
    for entry in &audit.experiments {
        out.push_str(&format!("{}\n", heading(entry)));
        out.push_str(&format!(
            "  指標名 {} 種類 — 説明あり {} ／ 説明なし {}\n",
            grouped(entry.n_names),
            grouped(entry.n_described()),
            grouped(entry.undescribed.len())
        ));
        if entry.n_by_name > 0 {
            out.push_str(&format!("  名前の宣言 {} 件\n", grouped(entry.n_by_name)));
        }
        if entry.n_by_vocabulary > 0 {
            out.push_str(&format!("  予約語 {} 件\n", grouped(entry.n_by_vocabulary)));
        }
        for family in &entry.families {
            let meaning = family
                .meaning
                .as_deref()
                .map(|m| format!(" — {m}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  形の宣言 {}  {} 件{}\n",
                family.pattern,
                grouped(family.n_matched),
                meaning
            ));
        }
        if !entry.undescribed.is_empty() {
            let (names, cut) = shown(&entry.undescribed, limit);
            if cut {
                out.push_str(&format!(
                    "  説明なし {} 件のうち {} 件を出しています\n",
                    grouped(entry.undescribed.len()),
                    grouped(names.len())
                ));
            } else {
                out.push_str(&format!("  説明なし {} 件\n", grouped(names.len())));
            }
            for name in names {
                out.push_str(&format!("    {name}\n"));
            }
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "合計 {} 実験 — 説明あり {} ／ 説明なし {}（実験ごとに数えた延べ）\n",
        grouped(audit.experiments.len()),
        grouped(audit.n_described()),
        grouped(audit.n_undescribed())
    ));
    out
}

/// The same count, for something that is not a person.
pub fn to_json(audit: &Audit, limit: usize) -> Value {
    let experiments: Vec<Value> = audit
        .experiments
        .iter()
        .map(|entry| {
            let (names, _) = shown(&entry.undescribed, limit);
            json!({
                "repo_id": entry.repo_id,
                "experiment": entry.experiment,
                "n_names": entry.n_names,
                "n_described": entry.n_described(),
                "n_undescribed": entry.undescribed.len(),
                "n_by_name": entry.n_by_name,
                "n_by_vocabulary": entry.n_by_vocabulary,
                "families": entry
                    .families
                    .iter()
                    .map(|f| json!({
                        "pattern": f.pattern,
                        "meaning": f.meaning,
                        "n_matched": f.n_matched,
                    }))
                    .collect::<Vec<Value>>(),
                "undescribed": names,
                "n_undescribed_shown": names.len(),
            })
        })
        .collect();
    json!({
        "schema_version": "1.0",
        "experiments": experiments,
        "totals": {
            "n_experiments": audit.experiments.len(),
            "n_described": audit.n_described(),
            "n_undescribed": audit.n_undescribed(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_five_figure_count_is_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(11_309), "11,309");
    }

    #[test]
    fn the_list_is_cut_only_when_it_is_longer_than_the_limit() {
        let names: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
        assert_eq!(shown(&names, 5), (&names[..], false));
        assert_eq!(shown(&names, 2), (&names[..2], true));
        // 0 は «全部» であって «1 件も出さない» ではない．
        assert_eq!(shown(&names, 0), (&names[..], false));
    }
}
