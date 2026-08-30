//! The core vocabulary, read from `schema/v1/vocabulary.toml` at compile time.
//!
//! Embedding the file rather than restating it in Rust is what keeps
//! `run.json`'s `vocab_version` from drifting away from the registry, and keeps
//! the reserved metric names in one place.

use std::collections::BTreeMap;
use std::sync::LazyLock;

const SOURCE: &str = include_str!("../../../schema/v1/vocabulary.toml");

/// A reserved metric name and the scopes it is allowed at.
#[derive(Debug, Clone)]
pub struct ReservedMetric {
    /// Which `scope` values the name may appear with.
    pub scopes: Vec<String>,
    /// What the number means.
    pub meaning: String,
}

/// The parsed registry.
#[derive(Debug)]
pub struct Vocabulary {
    /// The registry's own version, written into every `run.json`.
    pub version: String,
    /// Recommended `domain` values.
    pub domains: Vec<String>,
    /// Recommended `data[].role` values.
    pub data_roles: Vec<String>,
    /// Recommended `scope` values.
    pub scopes: Vec<String>,
    /// Recommended `step_unit` / `t_unit` values.
    pub step_units: Vec<String>,
    /// Core event kinds.
    pub event_schemas: Vec<String>,
    /// Reserved metric names, whose meaning is fixed.
    pub metric_names: BTreeMap<String, ReservedMetric>,
    /// Default freshness threshold for the dashboard report, in hours.
    pub freshness_hours: f64,
    /// Default cap on the number of runs in the dashboard report.
    pub max_runs: u64,
    /// Retired metric names that became another name: old to new.
    ///
    /// The rows keep their place in `metrics`; only the name changes.
    pub renamed_metrics: BTreeMap<String, String>,
    /// Retired metric names whose value belongs in a column: old to `<table>.<column>`.
    ///
    /// The row leaves `metrics` altogether. Where both the column and the
    /// retired metric hold a value, the column is the one that is kept.
    pub moved_metrics: BTreeMap<String, String>,
}

static VOCABULARY: LazyLock<Vocabulary> = LazyLock::new(|| {
    let doc: toml::Value = toml::from_str(SOURCE).expect("vocabulary.toml is valid TOML");

    let values = |table: &str| -> Vec<String> {
        doc.get(table)
            .and_then(|t| t.get("values"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    let metric_names = doc
        .get("metric_names")
        .and_then(|t| t.as_table())
        .map(|table| {
            table
                .iter()
                .map(|(name, spec)| {
                    let scopes = spec
                        .get("scope")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let meaning = spec
                        .get("meaning")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    (name.clone(), ReservedMetric { scopes, meaning })
                })
                .collect()
        })
        .unwrap_or_default();

    let report = |key: &str| doc.get("report").and_then(|t| t.get(key));

    let mapping = |kind: &str| -> BTreeMap<String, String> {
        doc.get("deprecated")
            .and_then(|t| t.get(kind))
            .and_then(|t| t.as_table())
            .map(|table| {
                table
                    .iter()
                    .filter_map(|(from, to)| to.as_str().map(|to| (from.clone(), to.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };

    Vocabulary {
        version: doc
            .get("vocab_version")
            .and_then(|v| v.as_str())
            .expect("vocabulary.toml declares vocab_version")
            .to_string(),
        domains: values("domains"),
        data_roles: values("data_roles"),
        scopes: values("scopes"),
        step_units: values("step_units"),
        event_schemas: values("event_schemas"),
        metric_names,
        freshness_hours: report("freshness_hours")
            .and_then(|v| v.as_float())
            .unwrap_or(24.0),
        max_runs: report("max_runs")
            .and_then(|v| v.as_integer())
            .unwrap_or(200) as u64,
        renamed_metrics: mapping("renamed"),
        moved_metrics: mapping("moved"),
    }
});

/// The core vocabulary.
pub fn get() -> &'static Vocabulary {
    &VOCABULARY
}

impl Vocabulary {
    /// Whether a metric name may be written at this scope.
    ///
    /// Only the reserved names are constrained: an experiment's own metric can
    /// be recorded at any scope.
    pub fn metric_allowed_at(&self, name: &str, scope: &str) -> bool {
        match self.metric_names.get(name) {
            Some(reserved) => reserved.scopes.iter().any(|s| s == scope),
            None => true,
        }
    }
}

/// What the index should do with one metric name.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolved<'a> {
    /// Keep the row under this name. Unchanged names resolve to themselves.
    Keep(&'a str),
    /// Drop the row from `metrics`; the value belongs in `<table>.<column>`.
    MoveTo {
        /// The table that owns the column.
        table: &'a str,
        /// The column the value belongs in.
        column: &'a str,
    },
    /// The registry names a destination this reader cannot place the value in.
    Unplaceable(&'a str),
}

impl Vocabulary {
    /// Where a metric name belongs once the retired vocabulary is applied.
    ///
    /// This is asked once, when `runvault query --refresh` builds the index, so
    /// that a query never has to know which names used to be something else.
    pub fn resolve_metric<'a>(&'a self, name: &'a str) -> Resolved<'a> {
        if let Some(destination) = self.moved_metrics.get(name) {
            return match destination.split_once('.') {
                Some((table, column)) if !table.is_empty() && !column.is_empty() => {
                    Resolved::MoveTo { table, column }
                }
                // A destination that is not `<table>.<column>` cannot be filled
                // in, and pretending otherwise would drop the value silently.
                _ => Resolved::Unplaceable(destination),
            };
        }
        Resolved::Keep(self.renamed_metrics.get(name).map_or(name, String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_parses_and_carries_a_version() {
        let v = get();
        assert!(v.version.contains('.'), "{}", v.version);
        assert!(v.domains.contains(&"simulation".to_string()));
        assert!(v.event_schemas.contains(&"observation".to_string()));
        assert!(v.scopes.contains(&"run".to_string()));
        assert!(v.step_units.contains(&"turn".to_string()));
        assert!(v.data_roles.contains(&"train".to_string()));
    }

    #[test]
    fn cost_is_a_run_level_number_only() {
        let v = get();
        assert!(v.metric_allowed_at("cost_usd", "run"));
        assert!(!v.metric_allowed_at("cost_usd", "trial"));
        assert!(v.metric_allowed_at("tokens_in", "trial"));
    }

    #[test]
    fn a_name_of_the_experiments_own_is_unconstrained() {
        assert!(get().metric_allowed_at("segregation_index", "agent"));
    }

    #[test]
    fn the_report_defaults_come_from_the_registry() {
        let v = get();
        assert!(v.freshness_hours > 0.0);
        assert!(v.max_runs > 0);
    }

    #[test]
    fn a_name_the_registry_does_not_retire_stays_as_it_is() {
        assert_eq!(
            get().resolve_metric("segregation_index"),
            Resolved::Keep("segregation_index")
        );
    }

    #[test]
    fn a_name_that_became_a_column_leaves_the_metrics_table() {
        // The registry retires `wall_sec` in favour of `status.json`, which is
        // the record of how long a run took.
        assert_eq!(
            get().resolve_metric("wall_sec"),
            Resolved::MoveTo {
                table: "runs",
                column: "duration_sec"
            }
        );
    }

    #[test]
    fn a_destination_that_is_not_a_column_is_refused_rather_than_guessed() {
        let vocabulary = Vocabulary {
            version: "1.0".into(),
            domains: Vec::new(),
            data_roles: Vec::new(),
            scopes: Vec::new(),
            step_units: Vec::new(),
            event_schemas: Vec::new(),
            metric_names: BTreeMap::new(),
            freshness_hours: 24.0,
            max_runs: 200,
            renamed_metrics: BTreeMap::from([("asr_old".into(), "asr".into())]),
            moved_metrics: BTreeMap::from([("elapsed".into(), "somewhere".into())]),
        };
        assert_eq!(vocabulary.resolve_metric("asr_old"), Resolved::Keep("asr"));
        assert_eq!(
            vocabulary.resolve_metric("elapsed"),
            Resolved::Unplaceable("somewhere")
        );
    }

    #[test]
    fn a_renamed_name_keeps_its_row_under_the_new_name() {
        let vocabulary = Vocabulary {
            version: "1.0".into(),
            domains: Vec::new(),
            data_roles: Vec::new(),
            scopes: Vec::new(),
            step_units: Vec::new(),
            event_schemas: Vec::new(),
            metric_names: BTreeMap::new(),
            freshness_hours: 24.0,
            max_runs: 200,
            renamed_metrics: BTreeMap::from([("segregation".into(), "segregation_index".into())]),
            moved_metrics: BTreeMap::new(),
        };
        // Renaming is not moving: the value is still a metric, and a query that
        // asks for the new name has to find the old rows under it.
        assert_eq!(
            vocabulary.resolve_metric("segregation"),
            Resolved::Keep("segregation_index")
        );
    }
}
