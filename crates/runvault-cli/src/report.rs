//! `runvault report --obsidian` — the dashboard's payload.
//!
//! `runs.json` is a summary of the index, not a record: losing it costs one
//! command. `schema/v1/runs.report.json` is the contract between this writer
//! and the Emera component that reads it (design note §5.4), so every shape
//! decision here is answerable to that file.
//!
//! Nothing is filled in for a run that did not record it. A legacy run has no
//! `status.json`, so its `state` is `null` rather than `unfinished` — the
//! latter means "a run written against this specification that has no
//! `status.json`", which is a different thing from "we were never told".

use std::collections::BTreeMap;
use std::path::Path;

use duckdb::Connection;
use duckdb::types::Value as Sql;
use serde_json::{Map, Value, json};

use crate::index::INDEX_DIR;

/// The metric names whose meaning is fixed by the registry.
///
/// They are excluded when the dashboard picks what to show: `n_units` and
/// `cost_usd` appear in nearly every run, so ranking by frequency without this
/// would put bookkeeping at the top of every experiment and never the result.
fn is_reserved(name: &str) -> bool {
    runvault::vocabulary::get().metric_names.contains_key(name)
}

/// The absolute path of one index table, quoted for SQL.
fn table(vault_root: &Path, name: &str) -> String {
    let path = vault_root.join(INDEX_DIR).join(format!("{name}.parquet"));
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn text(value: &Sql) -> Option<String> {
    match value {
        Sql::Text(v) => Some(v.clone()),
        _ => None,
    }
}

fn number(value: &Sql) -> Option<f64> {
    match value {
        Sql::TinyInt(v) => Some(*v as f64),
        Sql::SmallInt(v) => Some(*v as f64),
        Sql::Int(v) => Some(*v as f64),
        Sql::BigInt(v) => Some(*v as f64),
        Sql::Float(v) => Some(*v as f64),
        Sql::Double(v) => Some(*v),
        Sql::Decimal(v) => v.to_string().parse().ok(),
        _ => None,
    }
}

fn integer(value: &Sql) -> Option<i64> {
    number(value).map(|v| v as i64)
}

fn flag(value: &Sql) -> Option<bool> {
    match value {
        Sql::Boolean(v) => Some(*v),
        _ => None,
    }
}

/// A timestamp column, back in the shape `common.json` requires.
///
/// The index holds UTC, so the offset written here is `Z` rather than the one
/// the run was recorded with; the original stays in `run.json`.
fn moment(value: &Sql) -> Option<String> {
    let micros = match value {
        Sql::Timestamp(unit, count) => {
            let count = *count;
            match unit {
                duckdb::types::TimeUnit::Second => count.checked_mul(1_000_000)?,
                duckdb::types::TimeUnit::Millisecond => count.checked_mul(1_000)?,
                duckdb::types::TimeUnit::Microsecond => count,
                duckdb::types::TimeUnit::Nanosecond => count / 1_000,
            }
        }
        _ => return None,
    };
    chrono::DateTime::from_timestamp_micros(micros).map(|at| at.to_rfc3339())
}

/// Runs one query and hands each row to `f` as a slice of values.
fn each_row(
    connection: &Connection,
    sql: &str,
    mut f: impl FnMut(&[Sql]) -> Result<(), String>,
) -> Result<(), String> {
    let mut statement = connection.prepare(sql).map_err(|e| format!("{sql}: {e}"))?;
    let mut rows = statement.query([]).map_err(|e| format!("{sql}: {e}"))?;
    while let Some(row) = rows.next().map_err(|e| format!("{sql}: {e}"))? {
        // Asked of the row rather than the statement: a statement that has not
        // run yet has no columns to count, and asking anyway panics.
        let values: Vec<Sql> = (0..row.as_ref().column_count())
            .map(|i| row.get::<usize, Sql>(i).unwrap_or(Sql::Null))
            .collect();
        f(&values)?;
    }
    Ok(())
}

/// Builds the dashboard payload from the index in `vault_root`.
pub fn build(vault_root: &Path) -> Result<Value, String> {
    let vocabulary = runvault::vocabulary::get();
    let connection = Connection::open_in_memory().map_err(|e| e.to_string())?;

    let runs_table = table(vault_root, "runs");
    let metrics_table = table(vault_root, "metrics");
    let reference_table = table(vault_root, "reference");
    let targets_table = table(vault_root, "run_targets");
    let jira_table = table(vault_root, "run_jira");

    let experiments = experiments(&connection, &runs_table, &metrics_table, &jira_table)?;
    let runs = runs(
        &connection,
        &runs_table,
        &metrics_table,
        &reference_table,
        &targets_table,
        &jira_table,
        vocabulary.max_runs,
    )?;
    let warnings = warnings(&connection, &runs_table)?;

    Ok(json!({
        "schema_version": "1.1",
        "vocab_version": vocabulary.version,
        "generated_at": chrono::Local::now().to_rfc3339(),
        "freshness_hours": vocabulary.freshness_hours,
        "experiments": experiments,
        "runs": runs,
        "warnings": warnings,
    }))
}

/// One entry per `(repo_id, experiment)`, counted over every run in the index.
///
/// The counts come from the whole index rather than the capped `runs` list, so
/// narrowing what the screen shows never changes what it says happened.
fn experiments(
    connection: &Connection,
    runs_table: &str,
    metrics_table: &str,
    jira_table: &str,
) -> Result<Vec<Value>, String> {
    // `experiment` is grouped as it is, `NULL` included: a legacy run written
    // straight into `results/` never recorded which experiment it belonged to,
    // and putting it under the repository's name would be inventing one.
    let mut out = Vec::new();
    let sql = format!(
        "SELECT repo_id, experiment, count(*) AS n_runs,
                count(*) FILTER (WHERE state = 'finished') AS n_finished,
                max(created_at) AS last_run_at
         FROM {runs_table} GROUP BY 1, 2 ORDER BY 1, 2"
    );
    each_row(connection, &sql, |row| {
        out.push(json!({
            "repo_id": text(&row[0]).unwrap_or_default(),
            "experiment": text(&row[1]),
            "n_runs": integer(&row[2]).unwrap_or(0),
            "n_finished": integer(&row[3]).unwrap_or(0),
            "last_run_at": moment(&row[4]),
            "primary_metrics": Value::Array(Vec::new()),
            "jira": Value::Array(Vec::new()),
            "cost_usd": Value::Null,
            "git_remote": Value::Null,
        }));
        Ok(())
    })?;

    // そのリポジトリの origin. `run.json` にしか無いので, legacy run しか無い実験は
    // null のままになる — 埋めずに「記録が無い」と分かる形で残す.
    let sql = format!(
        "SELECT repo_id, experiment, arg_max(git_remote, created_at) AS remote
         FROM {runs_table} WHERE git_remote IS NOT NULL GROUP BY 1, 2"
    );
    let mut remotes: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
    each_row(connection, &sql, |row| {
        if let Some(remote) = text(&row[2]) {
            remotes.insert((text(&row[0]).unwrap_or_default(), text(&row[1])), remote);
        }
        Ok(())
    })?;

    // The registry fixes what `cost_usd` means, which is why it can be summed
    // across experiments at all.
    let sql = format!(
        "SELECT r.repo_id, r.experiment, sum(m.value) AS cost
         FROM {runs_table} AS r JOIN {metrics_table} AS m USING (run_key)
         WHERE m.name = 'cost_usd' AND m.scope = 'run' GROUP BY 1, 2"
    );
    let mut cost: BTreeMap<(String, Option<String>), f64> = BTreeMap::new();
    each_row(connection, &sql, |row| {
        if let Some(value) = number(&row[2]) {
            cost.insert((text(&row[0]).unwrap_or_default(), text(&row[1])), value);
        }
        Ok(())
    })?;

    // The issue keys of the whole experiment, not of the runs that fit in the
    // report. `runs[]` is capped at `max_runs`, so a dashboard that groups by
    // research theme from `runs[]` alone loses every experiment whose runs fall
    // outside the newest N — not as a smaller count, but as a repository that
    // is simply absent, which reads as "nothing was ever run here". Carrying
    // the keys here lets the grouping be built from the full set.
    //
    // Keys are collected across every run of the experiment: a run written
    // before the key was set has none, and one run carrying it is enough to say
    // which issue the experiment belongs to.
    let sql = format!(
        "SELECT r.repo_id, r.experiment, j.issue_key
         FROM {runs_table} AS r JOIN {jira_table} AS j USING (run_key)
         GROUP BY 1, 2, 3 ORDER BY 1, 2, 3"
    );
    let mut jira: BTreeMap<(String, Option<String>), Vec<String>> = BTreeMap::new();
    each_row(connection, &sql, |row| {
        let Some(issue) = text(&row[2]) else {
            return Ok(());
        };
        jira.entry((text(&row[0]).unwrap_or_default(), text(&row[1])))
            .or_default()
            .push(issue);
        Ok(())
    })?;

    let sql = format!(
        "SELECT r.repo_id, r.experiment, m.name, count(*) AS n
         FROM {runs_table} AS r JOIN {metrics_table} AS m USING (run_key)
         WHERE m.scope = 'run' GROUP BY 1, 2, 3 ORDER BY 1, 2, n DESC, m.name"
    );
    let mut primary: BTreeMap<(String, Option<String>), Vec<String>> = BTreeMap::new();
    each_row(connection, &sql, |row| {
        let Some(name) = text(&row[2]) else {
            return Ok(());
        };
        // §5.4 ②: the run-scope metrics, most common first. The registry's own
        // names are left out — see `is_reserved`.
        if is_reserved(&name) || runvault::ids::validate_slug("metric", &name).is_err() {
            return Ok(());
        }
        let entry = primary
            .entry((text(&row[0]).unwrap_or_default(), text(&row[1])))
            .or_default();
        if entry.len() < 3 {
            entry.push(name);
        }
        Ok(())
    })?;

    for experiment in &mut out {
        let key = (
            experiment["repo_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            experiment["experiment"].as_str().map(str::to_string),
        );
        if let Some(names) = primary.get(&key) {
            experiment["primary_metrics"] = json!(names);
        }
        if let Some(keys) = jira.get(&key) {
            experiment["jira"] = json!(keys);
        }
        if let Some(value) = cost.get(&key) {
            experiment["cost_usd"] = json!(value);
        }
        if let Some(remote) = remotes.get(&key) {
            experiment["git_remote"] = json!(remote);
        }
    }
    Ok(out)
}

/// The most recent runs, newest first, capped by the registry's `max_runs`.
fn runs(
    connection: &Connection,
    runs_table: &str,
    metrics_table: &str,
    reference_table: &str,
    targets_table: &str,
    jira_table: &str,
    max_runs: u64,
) -> Result<Vec<Value>, String> {
    let sql = format!(
        "SELECT run_key, run_uid, run_slug, experiment, subcommand, state, created_at,
                duration_sec, git_dirty, title, work_id, obsidian_note, repo_id
         FROM {runs_table} WHERE created_at IS NOT NULL
         ORDER BY created_at DESC LIMIT {max_runs}"
    );
    let mut runs: Vec<Value> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    each_row(connection, &sql, |row| {
        let key = text(&row[0]).unwrap_or_default();
        let Some(created_at) = moment(&row[6]) else {
            // `created_at` is required by the contract, so a run without one is
            // left out of the list rather than given a time it never had.
            return Ok(());
        };
        let mut entry = Map::new();
        entry.insert("run_key".into(), json!(key));
        // どのリポジトリの run かは，集約先での置き場を引くのに要る
        // (画面が run ディレクトリを開いて条件やハッシュを出すため)．
        entry.insert("repo_id".into(), json!(text(&row[12])));
        entry.insert("run_uid".into(), json!(text(&row[1])));
        entry.insert("run_slug".into(), json!(text(&row[2])));
        entry.insert("experiment".into(), json!(text(&row[3])));
        entry.insert("subcommand".into(), json!(text(&row[4])));
        entry.insert("state".into(), json!(text(&row[5])));
        entry.insert("created_at".into(), json!(created_at));
        entry.insert("duration_sec".into(), json!(number(&row[7])));
        entry.insert("git_dirty".into(), json!(flag(&row[8])));
        entry.insert("metrics".into(), json!({}));
        entry.insert("obsidian_note".into(), json!(text(&row[11])));
        entry.insert("jira".into(), json!([]));
        if let Some(title) = text(&row[9]) {
            let mut replication = Map::new();
            replication.insert("title".into(), json!(title));
            if let Some(work_id) = text(&row[10]) {
                replication.insert("work_id".into(), json!(work_id));
            }
            replication.insert("targets".into(), json!([]));
            entry.insert("replication".into(), Value::Object(replication));
        }
        runs.push(Value::Object(entry));
        keys.push(key);
        Ok(())
    })?;

    if runs.is_empty() {
        return Ok(runs);
    }
    let wanted = in_list(&keys);
    let at: BTreeMap<String, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| (key.clone(), i))
        .collect();

    // The run-scope aggregate, which is the number a table would carry. A point
    // on a series belongs to the series, not to the run.
    let sql = format!(
        "SELECT run_key, name, value FROM {metrics_table}
         WHERE scope = 'run' AND step IS NULL AND run_key IN ({wanted})"
    );
    each_row(connection, &sql, |row| {
        let (Some(key), Some(name), Some(value)) = (text(&row[0]), text(&row[1]), number(&row[2]))
        else {
            return Ok(());
        };
        if let Some(&i) = at.get(&key) {
            runs[i]["metrics"][name] = json!(value);
        }
        Ok(())
    })?;

    // The label alone does not identify a target. A figure with four rows is
    // four targets that all carry `Figure 8`, so selecting the label turned
    // them into four identical strings on screen — four distinct rows read as
    // one row recorded four times. Carry whatever tells them apart, and name
    // the field so a bare `2` stays readable. `target_id` orders them, since
    // the query is otherwise free to return them in any order.
    //
    // `condition` is the last resort: see the fallback below.
    let sql = format!(
        "SELECT run_key, label, panel, \"row\", condition FROM {targets_table}
         WHERE run_key IN ({wanted}) ORDER BY run_key, target_id"
    );
    each_row(connection, &sql, |row| {
        let (Some(key), Some(label)) = (text(&row[0]), text(&row[1])) else {
            return Ok(());
        };
        let mut display = label;
        let panel = text(&row[2]).filter(|v| !v.is_empty());
        let in_table = text(&row[3]).filter(|v| !v.is_empty());
        if let Some(panel) = &panel {
            display.push_str(&format!(" panel {panel}"));
        }
        if let Some(in_table) = &in_table {
            display.push_str(&format!(" row {in_table}"));
        }
        // A claim carries neither a panel nor a row: four claims can all be
        // labelled `section 2.2`, and only the condition says which is which.
        // Fall back to it rather than let them read as one claim recorded four
        // times. Where a panel or a row already tells them apart, the condition
        // is prose that belongs in the design note, so it is left out.
        if panel.is_none()
            && in_table.is_none()
            && let Some(condition) = text(&row[4]).filter(|v| !v.is_empty())
        {
            display.push_str(&format!(" — {condition}"));
        }
        if let Some(&i) = at.get(&key)
            && let Some(targets) = runs[i]
                .get_mut("replication")
                .and_then(|r| r.get_mut("targets"))
                .and_then(Value::as_array_mut)
        {
            targets.push(json!(display));
        }
        Ok(())
    })?;

    let sql = format!("SELECT run_key, issue_key FROM {jira_table} WHERE run_key IN ({wanted})");
    each_row(connection, &sql, |row| {
        let (Some(key), Some(issue)) = (text(&row[0]), text(&row[1])) else {
            return Ok(());
        };
        if let Some(&i) = at.get(&key)
            && let Some(jira) = runs[i].get_mut("jira").and_then(Value::as_array_mut)
        {
            jira.push(json!(issue));
        }
        Ok(())
    })?;

    // §5.2's third query: the reported value and the reproduced one meet only
    // when NULL steps are treated as equal, which `=` does not do.
    let sql = format!(
        "SELECT m.run_key, m.name, m.value - f.value AS diff
         FROM {metrics_table} AS m JOIN {reference_table} AS f
           ON  m.run_key = f.run_key AND m.name = f.name AND m.scope = f.scope
           AND m.step      IS NOT DISTINCT FROM f.step
           AND m.step_unit IS NOT DISTINCT FROM f.step_unit
         WHERE m.run_key IN ({wanted})"
    );
    each_row(connection, &sql, |row| {
        let (Some(key), Some(name), Some(diff)) = (text(&row[0]), text(&row[1]), number(&row[2]))
        else {
            return Ok(());
        };
        if let Some(&i) = at.get(&key)
            && let Some(replication) = runs[i].get_mut("replication")
        {
            if replication.get("diff").is_none() || replication["diff"].is_null() {
                replication["diff"] = json!({});
            }
            replication["diff"][name] = json!(diff);
        }
        Ok(())
    })?;

    Ok(runs)
}

/// A SQL list of the keys already selected, so the follow-up queries stay small.
fn in_list(keys: &[String]) -> String {
    keys.iter()
        .map(|key| format!("'{}'", key.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the screen should say out loud, beyond the numbers.
fn warnings(connection: &Connection, runs_table: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();

    // The same condition producing two environments is the case where two runs
    // that look comparable are not.
    let sql = format!(
        "SELECT config_hash, count(DISTINCT env_hash) AS n_env, count(*) AS n_run
         FROM {runs_table} WHERE state = 'finished' AND config_hash IS NOT NULL
         GROUP BY 1 HAVING n_env > 1 ORDER BY n_run DESC"
    );
    each_row(connection, &sql, |row| {
        let Some(config_hash) = text(&row[0]) else {
            return Ok(());
        };
        let n_run = integer(&row[2]).unwrap_or(0);
        out.push(json!({
            "kind": "env_split",
            "message": format!(
                "同じ config_hash の {n_run} run が {} 通りの環境で走っています",
                integer(&row[1]).unwrap_or(0)
            ),
            "config_hash": config_hash,
            "n_run": n_run,
        }));
        Ok(())
    })?;

    let sql =
        format!("SELECT count(*) FROM {runs_table} WHERE state = 'finished' AND git_dirty = true");
    each_row(connection, &sql, |row| {
        let n_run = integer(&row[0]).unwrap_or(0);
        if n_run > 0 {
            out.push(json!({
                "kind": "dirty_runs",
                "message": format!("{n_run} run が変更のある作業ツリーで走っており，論文の表には載せられません"),
                "n_run": n_run,
            }));
        }
        Ok(())
    })?;

    let sql = format!(
        "SELECT count(*), count(*) FILTER (WHERE experiment IS NULL)
         FROM {runs_table} WHERE run_uid IS NULL"
    );
    each_row(connection, &sql, |row| {
        let n_run = integer(&row[0]).unwrap_or(0);
        if n_run > 0 {
            let nameless = integer(&row[1]).unwrap_or(0);
            out.push(json!({
                "kind": "legacy_runs",
                "message": format!(
                    "{n_run} run はこの仕様より前に書かれたもので，うち {nameless} 件は実験名が記録されていません (実験ごとの集計では «実験名なし» に入ります)"
                ),
                "n_run": n_run,
            }));
        }
        Ok(())
    })?;

    Ok(out)
}
