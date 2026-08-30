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
}
