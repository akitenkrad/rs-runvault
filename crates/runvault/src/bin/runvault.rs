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
    #[arg(long)]
    latest: bool,
    /// Print every run whose `config_hash` starts with this prefix.
    #[arg(long)]
    config_hash: Option<String>,
}

#[derive(Args)]
struct VerifyArgs {
    /// The run directory.
    run: PathBuf,
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

    if let Some(prefix) = &args.config_hash {
        let mut found = 0;
        for dir in paths::run_dirs(&experiment_dir)? {
            let Ok(meta) = files::read_json::<RunMeta>(&dir.join("run.json")) else {
                continue;
            };
            if meta.config_hash.starts_with(prefix) {
                println!("{}", dir.display());
                found += 1;
            }
        }
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

fn relative_to_cwd(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
}
