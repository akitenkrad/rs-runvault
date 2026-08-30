//! The legacy reader against real run directories.
//!
//! `tests/fixtures/legacy/results/` is copied from the shapes that actually
//! exist in `social-simulation-replications`, so CI checks the reader against
//! what it will meet rather than against what the design note assumed. The whole
//! corpus (26 results directories, 243 runs) is swept by the ignored test at the
//! bottom, which needs that repository.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use runvault::legacy::{self, LegacyRun};

fn fixture_root() -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "legacy",
        "results",
    ]
    .iter()
    .collect()
}

fn run_by_relpath<'a>(runs: &'a [LegacyRun], relpath: &str) -> &'a LegacyRun {
    runs.iter()
        .find(|r| r.relpath == relpath)
        .unwrap_or_else(|| panic!("{relpath} was not read"))
}

/// The two keys the index joins on. A duplicate in either means the runs cannot
/// be aggregated, which is the whole point of reading them.
fn assert_keys_are_unique(runs: &[LegacyRun]) {
    let mut run_keys = HashSet::new();
    let mut metric_keys = HashSet::new();
    for run in runs {
        assert!(
            run_keys.insert(&run.run_key),
            "duplicate run_key: {}",
            run.run_key
        );
        for metric in &run.metrics {
            let key = (
                &run.run_key,
                &metric.name,
                metric.step,
                &metric.step_unit,
                &metric.scope,
            );
            assert!(
                metric_keys.insert(key),
                "duplicate metric key in {}: {} step={:?}",
                run.run_key,
                metric.name,
                metric.step
            );
        }
    }
}

#[test]
fn every_shape_in_the_fixture_corpus_is_read() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let found: Vec<&str> = runs.iter().map(|r| r.relpath.as_str()).collect();
    assert_eq!(
        found,
        [
            "20260530_223136",
            "20260620_134109",
            "20260620_162729_sweep",
            "20260620_165430",
            "paper_reproduction/20260530_203515",
            "paper_reproduction/20260717_111500",
            "reproduce_20260530_220555",
        ],
        "a per-condition subdirectory of a sweep must not become a run of its own"
    );
    assert_keys_are_unique(&runs);
}

#[test]
fn a_wide_run_keeps_its_parameters_and_its_time_series() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let run = run_by_relpath(&runs, "20260620_134109");

    assert_eq!(run.run_key, "legacy:schelling1971:20260620_134109");
    assert_eq!(run.timestamp.as_deref(), Some("20260620_134109"));
    assert_eq!(run.subcommand, None);
    assert_eq!(run.experiment, None);

    // config.json is a flat object here, not the envelope, and is kept as it is.
    let parameters = run.parameters.as_ref().expect("config.json");
    assert_eq!(parameters["rows"], 13);
    assert_eq!(parameters["threshold"], 0.5);
    assert_eq!(parameters["output_dir"], "results/20260620_134109");

    assert!(
        run.metrics
            .iter()
            .all(|m| m.step_unit.as_deref() == Some("step"))
    );
    let names: HashSet<&str> = run.metrics.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains("avg_same_ratio"));
    assert!(names.contains("dissimilarity_index"));
    assert!(run.tables.is_empty(), "{:?}", run.tables);
}

#[test]
fn a_sweep_keeps_its_summary_as_a_table_and_its_name_as_a_subcommand() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let run = run_by_relpath(&runs, "20260620_162729_sweep");

    assert_eq!(run.subcommand.as_deref(), Some("sweep"));
    assert!(run.metrics.is_empty());

    // One row per condition and no time axis: as long rows every row would claim
    // the same key, so the conditions would be lost rather than recorded.
    let table = run
        .tables
        .iter()
        .find(|t| t.file == "sweep_summary.csv")
        .expect("summary");
    assert!(table.rows >= 5, "{table:?}");
    assert!(table.header.contains(&"threshold".to_string()));
    assert!(table.reason.contains("主キー"), "{}", table.reason);

    assert!(run.extras.contains(&"sweep_config.json".to_string()));
}

#[test]
fn a_two_level_layout_records_the_grouping_directory_as_the_experiment() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let run = run_by_relpath(&runs, "paper_reproduction/20260530_203515");
    assert_eq!(run.experiment.as_deref(), Some("paper_reproduction"));
    assert_eq!(
        run.run_key,
        "legacy:schelling1971:paper_reproduction/20260530_203515"
    );

    // A run that holds only images still reads; it simply has nothing to add.
    let figures = run_by_relpath(&runs, "paper_reproduction/20260717_111500");
    assert!(figures.metrics.is_empty());
    assert!(figures.tables.is_empty());
    assert_eq!(figures.extras, ["bnm_group1.png"]);
}

#[test]
fn a_prefixed_name_is_read_as_a_subcommand() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let run = run_by_relpath(&runs, "reproduce_20260530_220555");
    assert_eq!(run.subcommand.as_deref(), Some("reproduce"));
    assert_eq!(run.timestamp.as_deref(), Some("20260530_220555"));
}

#[test]
fn a_run_whose_metrics_are_already_long_is_not_read_as_wide() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let run = run_by_relpath(&runs, "20260530_223136");

    let names: HashSet<&str> = run.metrics.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains("polarization_index"), "{names:?}");
    assert!(
        !names.contains("value"),
        "read as wide, the name column would be lost"
    );

    // A per-agent panel is listed, not turned into run-level numbers.
    assert!(run.extras.contains(&"opinions.csv".to_string()));
    assert!(run.extras.contains(&"run_metadata.json".to_string()));
}

#[test]
fn a_summary_without_a_time_axis_is_reported_rather_than_guessed() {
    let runs = legacy::read_all(&fixture_root(), "schelling1971").unwrap();
    let run = run_by_relpath(&runs, "reproduce_20260530_220555");
    assert!(run.metrics.is_empty());
    assert_eq!(run.tables.len(), 1);
    assert!(run.tables[0].header.contains(&"experiment".to_string()));
}

/// Sweeps the whole replication corpus. Needs the repository, so it is opt-in:
///
/// ```text
/// RUNVAULT_LEGACY_CORPUS=~/Documents/workspace/social-simulation-replications \
///     cargo test -p runvault --test legacy_corpus -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs the social-simulation-replications checkout"]
fn the_whole_replication_corpus_reads_without_key_collisions() {
    let Ok(root) = std::env::var("RUNVAULT_LEGACY_CORPUS") else {
        panic!("set RUNVAULT_LEGACY_CORPUS to the replications checkout");
    };
    let root = PathBuf::from(shellexpand(&root));

    let mut results_roots = Vec::new();
    collect_results_dirs(&root, &mut results_roots);
    assert!(
        !results_roots.is_empty(),
        "no results/ under {}",
        root.display()
    );

    let (mut runs, mut metrics, mut tables) = (0, 0, 0);
    for results in &results_roots {
        let repo_id = results
            .strip_prefix(&root)
            .unwrap_or(results)
            .to_string_lossy()
            .replace(['/', ' '], "-")
            .to_lowercase();
        let read = legacy::read_all(results, &repo_id)
            .unwrap_or_else(|e| panic!("{}: {e}", results.display()));
        assert_keys_are_unique(&read);
        runs += read.len();
        metrics += read.iter().map(|r| r.metrics.len()).sum::<usize>();
        tables += read.iter().map(|r| r.tables.len()).sum::<usize>();
    }
    println!(
        "results {} 件 / run {runs} 件 / 指標 {metrics} 行 / 表のまま {tables} 件",
        results_roots.len()
    );

    // Lower bounds, not just "it did not crash": a reader that quietly stopped
    // converting anything would otherwise sweep the whole corpus and pass.
    assert!(runs > 200, "only {runs} runs were found");
    assert!(
        metrics > 20_000,
        "only {metrics} metric rows were read; the corpus yielded 21,857 on 2026-08-30"
    );
    assert!(
        tables < runs,
        "{tables} tables against {runs} runs: most runs stopped converting"
    );
}

fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.to_string(),
    }
}

fn collect_results_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if name == "results" {
            out.push(path);
        } else {
            collect_results_dirs(&path, out);
        }
    }
}
