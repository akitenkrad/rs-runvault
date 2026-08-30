//! The `runvault` command line.
//!
//! Phase 1 covers the subcommands that operate on a single machine's run
//! directories: finding one, checking one, and cleaning up after a killed one.
//! `sync`, `query` and `report` arrive with the aggregation layer.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use runvault::gc::Outcome;
use runvault::meta::RunMeta;
use runvault::{Result, files, paths, verify};

#[derive(Parser)]
#[command(name = "runvault", version, about = "Plain-file experiment tracking")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print run directories: the latest finished one, or the ones sharing a condition.
    Path(PathArgs),
    /// Check a run directory against the invariants that span its files.
    Verify(VerifyArgs),
    /// Turn runs whose process was killed into recorded failures.
    Gc(GcArgs),
    /// Read run directories written before this specification existed.
    Legacy(LegacyArgs),
}

#[derive(Args)]
struct PathArgs {
    /// The experiment to look in.
    #[arg(long)]
    experiment: String,
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// Resolve the `latest_finished` link.
    #[arg(long, conflicts_with_all = ["config_hash", "execution_hash"])]
    latest: bool,
    /// Print every run whose `config_hash` starts with this prefix (the same condition).
    #[arg(long)]
    config_hash: Option<String>,
    /// Print every run whose `execution_hash` starts with this prefix.
    ///
    /// This is what answers "has this exact thing already been run": the same
    /// condition, the same seeds, the same commit and the same environment.
    #[arg(long)]
    execution_hash: Option<String>,
    /// Only consider runs that finished. A failed run is not a run that happened.
    #[arg(long)]
    finished: bool,
}

#[derive(Args)]
struct VerifyArgs {
    /// The run directory.
    run: PathBuf,
}

#[derive(Args)]
struct LegacyArgs {
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// The stable repository id the keys are built from.
    #[arg(long)]
    repo_id: String,
    /// Print the runs as JSON instead of a summary.
    #[arg(long)]
    json: bool,
    /// Also print what each run could not convert.
    #[arg(long)]
    notes: bool,
}

#[derive(Args)]
struct GcArgs {
    /// Where the experiment directories live.
    #[arg(long, default_value = "results")]
    results_root: PathBuf,
    /// Report what would happen without writing anything.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Path(args) => cmd_path(&args),
        Command::Verify(args) => cmd_verify(&args),
        Command::Gc(args) => cmd_gc(&args),
        Command::Legacy(args) => cmd_legacy(&args),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("runvault: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_path(args: &PathArgs) -> Result<ExitCode> {
    let experiment_dir = paths::experiment_dir(&args.results_root, &args.experiment);

    if args.config_hash.is_some() || args.execution_hash.is_some() {
        let mut found = 0;
        for dir in paths::run_dirs(&experiment_dir)? {
            let Ok(meta) = files::read_json::<RunMeta>(&dir.join("run.json")) else {
                continue;
            };
            let matches = |prefix: &Option<String>, hash: &str| {
                prefix.as_ref().is_none_or(|p| hash.starts_with(p))
            };
            if !matches(&args.config_hash, &meta.config_hash)
                || !matches(&args.execution_hash, &meta.execution_hash)
            {
                continue;
            }
            if args.finished && !is_finished(&dir) {
                continue;
            }
            println!("{}", dir.display());
            found += 1;
        }
        // Nothing found is a failing exit code so a shell script can branch on it
        // without parsing the output.
        return Ok(if found > 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    if args.latest {
        let link = experiment_dir.join(paths::LATEST_FINISHED);
        if !link.exists() {
            eprintln!("runvault: {} がありません", link.display());
            return Ok(ExitCode::FAILURE);
        }
        println!("{}", std::fs::canonicalize(&link)?.display());
        return Ok(ExitCode::SUCCESS);
    }

    for dir in paths::run_dirs(&experiment_dir)? {
        println!("{}", dir.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether a run directory holds a `status.json` that says it finished.
fn is_finished(dir: &Path) -> bool {
    files::read_json::<runvault::RunStatus>(&dir.join("status.json"))
        .is_ok_and(|status| status.state == runvault::State::Finished)
}

fn cmd_verify(args: &VerifyArgs) -> Result<ExitCode> {
    match verify::shallow(&args.run) {
        Ok(()) => {
            println!("ok {}", args.run.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{}: {e}", args.run.display());
            Ok(ExitCode::FAILURE)
        }
    }
}

fn cmd_gc(args: &GcArgs) -> Result<ExitCode> {
    let reaped = runvault::gc::collect(&args.results_root, args.dry_run)?;
    let mut stale = 0;
    for entry in &reaped {
        let label = match entry.outcome {
            Outcome::Running => "running",
            Outcome::Contested => "running (一度 stale と判定したが生きていた)",
            Outcome::Reaped => {
                stale += 1;
                if args.dry_run {
                    "stale (dry-run)"
                } else {
                    "reaped"
                }
            }
        };
        println!("{label}\t{}", relative_to_cwd(&entry.dir).display());
    }
    println!("{} 件を確認し，{stale} 件が異常終了でした", reaped.len());
    Ok(ExitCode::SUCCESS)
}

fn cmd_legacy(args: &LegacyArgs) -> Result<ExitCode> {
    let runs = runvault::legacy::read_all(&args.results_root, &args.repo_id)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(ExitCode::SUCCESS);
    }

    // The index keys on (run_key) and (run_key, name, step, step_unit, scope);
    // if those are not unique the runs cannot be joined, so it is checked here
    // rather than discovered when the index is built.
    let mut keys = std::collections::HashSet::new();
    let mut metric_keys = std::collections::HashSet::new();
    let mut collisions = 0;
    let (mut metrics, mut tables) = (0, 0);

    for run in &runs {
        if !keys.insert(run.run_key.clone()) {
            eprintln!("run_key が重複しています: {}", run.run_key);
            collisions += 1;
        }
        for metric in &run.metrics {
            let key = (
                run.run_key.clone(),
                metric.name.clone(),
                metric.step,
                metric.step_unit.clone(),
                metric.scope.clone(),
            );
            if !metric_keys.insert(key) {
                eprintln!(
                    "指標の主キーが重複しています: {} {}",
                    run.run_key, metric.name
                );
                collisions += 1;
            }
        }
        metrics += run.metrics.len();
        tables += run.tables.len();

        println!(
            "{}\t{} metrics\t{} tables\t{}",
            run.timestamp.as_deref().unwrap_or("-"),
            run.metrics.len(),
            run.tables.len(),
            run.relpath
        );
        if args.notes {
            for note in &run.notes {
                println!("  note: {note}");
            }
            for table in &run.tables {
                println!(
                    "  table: {} ({} 行) — {}",
                    table.file, table.rows, table.reason
                );
            }
        }
    }

    println!(
        "\nrun {} 件・指標 {metrics} 行・変換しなかった表 {tables} 件",
        runs.len()
    );
    if collisions > 0 {
        eprintln!("主キーの重複 {collisions} 件");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn relative_to_cwd(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
}
