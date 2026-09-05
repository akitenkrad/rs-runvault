//! Progress reporting for subcommands that can run for more than a minute.
//!
//! A `sweep` in one of the replication repositories ran for ninety-five minutes
//! without printing a line after its first three. Nothing in the output said
//! whether it was computing or wedged, and the two were told apart with `ps` and
//! the process's accumulated CPU time — a diagnosis made outside the program,
//! and made wrong first.
//!
//! Buffering was not the cause and is not what this module fixes. std's `Stdout`
//! is a `LineWriter` whether or not it is a terminal, so those three lines did
//! reach the file; the run was silent because nothing further was printed at
//! all. What was missing was output at the granularity of the *work*, and that
//! is what a [`Stage`] is:
//!
//! * **The denominator.** A line carries `done/total`, the elapsed time and an
//!   estimate of the time left. "Entered stage 2" does not distinguish a stage
//!   that takes a minute from one that takes three hours.
//! * **The stream.** Lines go to **standard error**, leaving standard output for
//!   the run's machine-readable results, and the same lines are mirrored into
//!   [`LOG_PATH`] inside the run directory, where `manifest.csv` hashes them and
//!   the record outlives the terminal the run was started from.
//! * **The flush.** Every line is flushed as it is written, to both sinks, and
//!   `isatty` is deliberately **not** consulted: a run whose output is redirected
//!   is exactly the run whose progress is worth having.
//! * **The rate.** One line per condition would bury a run in its own log, so a
//!   stage reports every [`STEP_FRACTION`] of its total — and, whichever comes
//!   first, at least every [`REPORT_INTERVAL`], so that no stage can be silent
//!   for the span that prompted this module.
//! * **The estimate's denominator.** A stage whose conditions cost the same can
//!   extrapolate from the count. One whose conditions span orders of magnitude
//!   cannot, and a count-based estimate there says "19s" with half an hour to
//!   go. Such a stage is opened with a cost per condition
//!   ([`Progress::weighted_stage`]) and both the percentage and the estimate are
//!   read off that. An estimate that is confidently wrong is the failure this
//!   module exists to fix, not a smaller version of it.
//! * **The unknown total.** A stage that cannot count its work ahead of time
//!   ([`Progress::unbounded_stage`]) reports the count it has reached on a timer
//!   and carries no percentage and no estimate, rather than inventing a
//!   denominator.
//!
//! Progress is **not a metric**. `metrics.csv` holds the quantities an
//! experiment is about; how long a run took is `status.json`'s `duration_sec`,
//! and a second answer to that question in another file is a second answer that
//! can disagree. Nothing here returns a duration for a caller to log, and
//! [`tests::a_stage_writes_nowhere_but_its_log`] holds the module to it.
//!
//! # What is common and what is the caller's
//!
//! **Common — this module.** The reporting rate, the elapsed time and the
//! estimate, the share-of-the-work arithmetic, one flushed line per write to
//! standard error, and the copy under `logs/`. Callers never format a line,
//! never choose a stream and never decide when to report; two dozen repositories
//! that each did would be two dozen spellings to read across.
//!
//! **The caller's.** Which stages there are and what they are called, what
//! counts as one condition in each of them, and the cost model an uneven stage
//! is weighted by. None of that generalises.
//!
//! # Using it
//!
//! ```no_run
//! # fn main() -> runvault::Result<()> {
//! # let conditions: Vec<u32> = Vec::new();
//! # let mut run = runvault::Run::start(
//! #     runvault::RunOptions::new("sweep", "sweep")
//! #         .repo_id("r").domain("other")
//! #         .origin(runvault::Origin::Manual)
//! #         .parameters(&serde_json::json!({}))?,
//! # )?;
//! let mut stage = run.stage("stage 2", conditions.len());
//! for condition in &conditions {
//!     // ... the work, and any metrics it produces ...
//!     stage.tick();
//! }
//! stage.close();
//! run.finish()?;
//! # Ok(())
//! # }
//! ```
//!
//! A [`Stage`] borrows nothing, so metrics can still be recorded inside the loop
//! it reports on. Close it before [`Run::finish`](crate::Run::finish): the
//! manifest is written there, and a line added afterwards is a line the manifest
//! disagrees with. A stage left open past the end of its run keeps reporting to
//! standard error and stops writing to the run, saying so once — the mirror is
//! dropped rather than the record broken.
//!
//! A borrow would have made that a compile error instead, and it was tried:
//! `Stage<'a>` holding `&'a Run` is `E0502` against `run.log_metric(..)` in the
//! very loop the stage reports on, because logging takes `&mut Run`. Interior
//! mutability would buy the borrow back at the price of `Run: Sync`, which
//! parallel sweeps rely on. Reporting while working is what a progress API is
//! for, so the ordering is checked at run time and the guarantee — that nothing
//! is appended to a sealed `manifest.csv` — is kept either way.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How much of a stage must finish between two progress lines.
///
/// Five per cent: twenty lines for a stage, which is enough to see the rate and
/// to see it change, and few enough that the log of a whole sweep still fits on
/// a screen.
pub const STEP_FRACTION: f64 = 0.05;

/// The longest a stage may go without a line, whatever its step works out to.
///
/// Five per cent of a stage is a share of the work, not a span of time; a stage
/// of twenty slow conditions would still be silent for minutes at a stretch.
/// This is the ceiling on that silence, and the only rule an unbounded stage
/// has.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// The file inside the run directory that the same lines are written to.
///
/// Under `logs/`, because `finish()` walks exactly `artifacts/` and `logs/` into
/// `manifest.csv`.
pub const LOG_PATH: &str = "logs/progress.log";

/// Opens the stages of one subcommand.
///
/// Cheap to make and to keep: it holds where the lines are mirrored, not an open
/// file, so several stages may be alive at once and each writes its own lines.
///
/// [`Run::stage`](crate::Run::stage) and its siblings build one of these for the
/// run they are called on, which is the usual way in. Construct one directly
/// only for a subcommand that has no run to write into.
#[derive(Clone, Debug)]
pub struct Progress {
    /// Where the lines are mirrored, when they are mirrored anywhere.
    log: Option<PathBuf>,
    /// Cleared when the run closes; `None` when the lines answer to no run.
    open: Option<Arc<AtomicBool>>,
}

impl Progress {
    /// Progress on standard error only.
    pub fn to_stderr() -> Self {
        Progress {
            log: None,
            open: None,
        }
    }

    /// Progress mirrored into `{dir}/logs/progress.log`.
    ///
    /// Lines are appended, so a second `Progress` over the same directory
    /// continues the log rather than replacing it.
    pub fn in_run(dir: &Path) -> Self {
        Progress {
            log: Some(dir.join(LOG_PATH)),
            open: None,
        }
    }

    /// Mirrored into a run that can close underneath the caller.
    ///
    /// `open` is cleared when the run writes its manifest, after which the
    /// mirror is dropped: the run's record has already been sealed, and a line
    /// appended past it is a digest the manifest disagrees with.
    pub(crate) fn in_open_run(dir: &Path, open: Arc<AtomicBool>) -> Self {
        Progress {
            log: Some(dir.join(LOG_PATH)),
            open: Some(open),
        }
    }

    /// Opens a stage of `total` equally costly conditions.
    ///
    /// `total` is the number of units [`Stage::tick`] will be called for. A
    /// stage of zero units reports its opening and its closing and nothing
    /// between, which is the honest reading of a stage that was skipped.
    pub fn stage(&self, name: &str, total: usize) -> Stage {
        self.open_stage(name, vec![1.0; total], Some(total))
    }

    /// Opens a stage whose conditions cost different amounts.
    ///
    /// `costs` holds one figure per condition, **in the order they will be
    /// ticked**, in any unit proportional to the time a condition takes; only
    /// the ratios are read. Lines still arrive every [`STEP_FRACTION`] of the
    /// *count*, so they keep coming at an even rate, but the percentage and the
    /// estimate are shares of the cost.
    pub fn weighted_stage(&self, name: &str, costs: Vec<f64>) -> Stage {
        let total = costs.len();
        self.open_stage(name, costs, Some(total))
    }

    /// Opens a stage whose total is not known before it runs.
    ///
    /// It reports the count it has reached every [`REPORT_INTERVAL`], and
    /// carries neither a percentage nor an estimate: both need a denominator,
    /// and a guessed one is the failure this module exists to prevent. Prefer
    /// [`Progress::stage`] whenever the work can be counted first — a share is
    /// worth more than a tally.
    pub fn unbounded_stage(&self, name: &str) -> Stage {
        self.open_stage(name, Vec::new(), None)
    }

    fn open_stage(&self, name: &str, costs: Vec<f64>, total: Option<usize>) -> Stage {
        // A cost that is not a positive number is a cost model that failed, and
        // keeping it silently would put a NaN into every percentage from here
        // on. Fall back to counting, which is wrong by less.
        let costs = if costs.iter().all(|c| c.is_finite() && *c > 0.0) {
            costs
        } else {
            vec![1.0; costs.len()]
        };
        let step = total.map_or(1, step_for);
        let now = Instant::now();
        let mut stage = Stage {
            file: self.open_log(),
            open: self.open.clone(),
            name: name.to_string(),
            cost_total: costs.iter().sum(),
            costs,
            cost_done: 0.0,
            total,
            done: 0,
            step,
            // An unbounded stage has no share to cross, so only the timer can
            // make it report.
            next: if total.is_some() { step } else { usize::MAX },
            started: now,
            last: now,
            closed: false,
        };
        stage.emit();
        stage
    }

    /// The mirror, when there is one and it can be opened.
    ///
    /// A directory that cannot be written costs the mirror, not the run: the
    /// standard-error half still reports, and the reason is said once.
    fn open_log(&self) -> Option<File> {
        let path = self.log.as_ref()?;
        let opened = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| OpenOptions::new().create(true).append(true).open(path));
        match opened {
            Ok(file) => Some(file),
            Err(e) => {
                let mut err = std::io::stderr();
                let _ = writeln!(err, "progress: cannot write {}: {e}", path.display());
                let _ = err.flush();
                None
            }
        }
    }
}

/// One counted stage of a subcommand.
///
/// Stages are counted **separately**: a fraction of a whole run would need the
/// stages' relative costs, which are not known before the run, and a wrong one
/// is worse than none.
///
/// A stage owns its half of the mirror and borrows nothing, so the run it
/// reports on stays free to record metrics inside the loop.
pub struct Stage {
    file: Option<File>,
    open: Option<Arc<AtomicBool>>,
    name: String,
    /// What each condition is expected to cost, in tick order. All ones for a
    /// stage opened with [`Progress::stage`], empty for an unbounded one.
    costs: Vec<f64>,
    cost_total: f64,
    cost_done: f64,
    /// The number of conditions, when it is known before the stage runs.
    total: Option<usize>,
    done: usize,
    step: usize,
    next: usize,
    started: Instant,
    last: Instant,
    closed: bool,
}

impl Stage {
    /// One condition finished.
    pub fn tick(&mut self) {
        self.cost_done += self.costs.get(self.done).copied().unwrap_or(1.0);
        self.done += 1;
        // The last condition is left to `close`, which says the same thing and
        // adds that the stage is over; reporting both would end every stage on
        // two identical hundred-per-cent lines.
        if self.total.is_some_and(|total| self.done >= total) {
            return;
        }
        if self.done >= self.next || self.last.elapsed() >= REPORT_INTERVAL {
            self.emit();
            // Aligned on multiples of the step rather than on the tick that
            // happened to cross it, so a stage whose ticks arrive in batches
            // still reports at five per cent and not at the batch boundary.
            while self.next <= self.done {
                self.next += self.step;
            }
        }
    }

    /// The stage is over. Reports the final count, whatever it reached.
    ///
    /// Separate from `Drop` so that the closing line is written where the stage
    /// ends rather than where the value happens to go out of scope.
    pub fn close(mut self) {
        self.closed = true;
        self.emit();
    }

    /// How many conditions have been counted so far.
    pub fn done(&self) -> usize {
        self.done
    }

    fn emit(&mut self) {
        let elapsed = self.started.elapsed();
        let head = match self.total {
            Some(total) => {
                // The share of the WORK, which for an even stage is the share of
                // the count and for an uneven one is not.
                let share = if self.cost_total > 0.0 {
                    self.cost_done / self.cost_total
                } else {
                    1.0
                };
                format!("{:>7}/{:<7} {:>3.0}%", self.done, total, 100.0 * share)
            }
            // Same columns, and `?` where the denominator would be: a total
            // nobody knows is reported as unknown, not as the count so far.
            None => format!("{:>7}/{:<7}     ", self.done, "?"),
        };
        let mut text = format!(
            "progress: {:<12} {head}  elapsed {:>8}",
            self.name,
            duration(elapsed)
        );
        // An estimate needs something to extrapolate from, so it appears from
        // the first completed condition and not before, and it is dropped again
        // on the closing line, where the answer is the elapsed time.
        if let Some(total) = self.total
            && self.done > 0
            && self.done < total
        {
            let per = elapsed.as_secs_f64() / self.cost_done.max(f64::MIN_POSITIVE);
            let left = per * (self.cost_total - self.cost_done).max(0.0);
            text.push_str(&format!(
                "  eta {:>8}",
                duration(Duration::from_secs_f64(left.max(0.0)))
            ));
        }
        if self.closed {
            text.push_str("  done");
        }
        self.line(&text);
        self.last = Instant::now();
    }

    fn line(&mut self, text: &str) {
        let mut err = std::io::stderr();
        // Both writes are flushed as they happen. `Stderr` is unbuffered in std
        // today, but the flush is what the contract is, not what the current
        // implementation happens to give.
        let _ = writeln!(err, "{text}");
        let _ = err.flush();
        // The run seals `manifest.csv` when it finishes. Past that point the
        // mirror is dropped and never reopened: a hashed file that grows
        // afterwards fails `runvault verify` for a reason that has nothing to do
        // with the experiment.
        //
        // Said out loud, once, on the way down. A stage outliving its run is a
        // call site that closes in the wrong order, and a mirror that quietly
        // stopped is how that goes unnoticed until someone asks why the log ends
        // where it does.
        if self.file.is_some()
            && self
                .open
                .as_ref()
                .is_some_and(|o| !o.load(Ordering::SeqCst))
        {
            self.file = None;
            let _ = writeln!(
                err,
                "progress: {} outlived its run; close() it before finish() to keep the rest of \
                 the stage in {LOG_PATH}",
                self.name
            );
            let _ = err.flush();
        }
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{text}");
            let _ = file.flush();
        }
    }
}

/// How many conditions between two lines, for a stage of `total` of them.
///
/// At least one, so a stage smaller than the step still reports.
pub fn step_for(total: usize) -> usize {
    ((total as f64 * STEP_FRACTION).ceil() as usize).max(1)
}

/// `1h02m03s` / `2m03s` / `3s`, whichever the magnitude asks for.
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m{:02}s", s / 3600, (s % 3600) / 60, s % 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twenty lines to a stage, and never zero — a step of zero would stop
    /// `next` advancing and the stage would report on every tick.
    #[test]
    fn the_step_is_five_per_cent_and_at_least_one() {
        assert_eq!(step_for(4000), 200);
        assert_eq!(step_for(100), 5);
        // Rounded up, so a stage never reports more often than every five per
        // cent.
        assert_eq!(step_for(41), 3);
        assert_eq!(step_for(6), 1);
        assert_eq!(step_for(1), 1);
        assert_eq!(step_for(0), 1);
    }

    #[test]
    fn durations_carry_the_unit_the_magnitude_asks_for() {
        assert_eq!(duration(Duration::from_secs(0)), "0s");
        assert_eq!(duration(Duration::from_secs(59)), "59s");
        assert_eq!(duration(Duration::from_secs(60)), "1m00s");
        assert_eq!(duration(Duration::from_secs(3599)), "59m59s");
        assert_eq!(duration(Duration::from_secs(3600)), "1h00m00s");
        assert_eq!(duration(Duration::from_secs(5696)), "1h34m56s");
    }

    /// The lines land in the run directory, under `logs/`, where `manifest.csv`
    /// will hash them.
    #[test]
    fn the_same_lines_are_written_into_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let progress = Progress::in_run(dir.path());
        let mut stage = progress.stage("stage 2", 40);
        for _ in 0..40 {
            stage.tick();
        }
        stage.close();

        let lines = log_lines(dir.path());
        // One on opening, nineteen at five per cent each, and one on closing —
        // which is the twentieth five-per-cent line as well, so the stage does
        // not end on two identical hundred-per-cent lines.
        assert_eq!(lines.len(), 21, "{lines:#?}");
        assert!(lines[0].contains("      0/40"), "{}", lines[0]);
        assert!(lines[0].contains("  0%"), "{}", lines[0]);
        assert!(lines[1].contains("      2/40"), "{}", lines[1]);
        assert!(lines[1].contains("  5%"), "{}", lines[1]);
        assert!(lines[1].contains("eta"), "{}", lines[1]);
        assert!(lines[19].contains("     38/40"), "{}", lines[19]);
        assert!(lines[20].contains("     40/40"), "{}", lines[20]);
        assert!(lines[20].contains("100%"), "{}", lines[20]);
        assert!(lines[20].ends_with("done"), "{}", lines[20]);
        // The estimate is of the time LEFT, so the line with none left does not
        // carry one.
        assert!(!lines[20].contains("eta"), "{}", lines[20]);
        for line in &lines {
            assert!(line.contains("elapsed"), "{line}");
        }
    }

    /// Progress is a log, not a measurement: nothing here may reach
    /// `metrics.csv`, whose `duration_sec` counterpart is `status.json`'s.
    #[test]
    fn a_stage_writes_nowhere_but_its_log() {
        let dir = tempfile::tempdir().unwrap();
        Progress::in_run(dir.path()).stage("stage 3", 3).close();
        let written: Vec<String> = walk(dir.path())
            .into_iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(written, vec![LOG_PATH.to_string()]);
    }

    /// The module is the one copy of this behaviour for every repository that
    /// records a run, so it may reach for `std` and for nothing else: coupling
    /// it to `Run`, to an error type or to a config would make it answerable to
    /// them. Checked on the source rather than left to a reviewer, because the
    /// line that breaks it is one line long and looks like every other line.
    ///
    /// `Run::stage` is the exception that proves it, and it points the other
    /// way: the run reaches for the progress module, not the reverse.
    #[test]
    fn the_module_depends_on_nothing_but_std() {
        let source = include_str!("progress.rs");
        let mut offenders: Vec<&str> = Vec::new();
        for line in source.lines() {
            let line = line.trim();
            if !line.starts_with("use ") {
                continue;
            }
            // `use super::*` is this file's own test module reaching at this
            // file. Everything else must name `std`.
            if line.starts_with("use std::") || line == "use super::*;" {
                continue;
            }
            offenders.push(line);
        }
        assert!(
            offenders.is_empty(),
            "the module has grown a dependency: {offenders:?}"
        );
    }

    /// The percentage of an uneven stage is the share of the WORK, not of the
    /// conditions. Twenty cheap conditions out of twenty-one are not ninety-five
    /// per cent of a stage whose twenty-first is most of it.
    #[test]
    fn an_uneven_stage_is_measured_by_cost() {
        let dir = tempfile::tempdir().unwrap();
        let mut costs = vec![1.0; 20];
        costs.push(980.0);
        let mut stage = Progress::in_run(dir.path()).weighted_stage("stage 1b", costs);
        for _ in 0..20 {
            stage.tick();
        }
        // Twenty of twenty-one conditions, two per cent of the work.
        assert_eq!(stage.done(), 20);
        stage.tick();
        stage.close();

        let lines = log_lines(dir.path());
        let at_20 = lines.iter().find(|l| l.contains("20/21")).unwrap();
        assert!(at_20.contains("  2%"), "{at_20}");
        assert!(lines.last().unwrap().contains("100%"), "{lines:#?}");
    }

    /// A cost model that produced a NaN, an infinity or a zero is a cost model
    /// that failed. Counting is wrong by less than a percentage that is NaN from
    /// the first tick onwards.
    #[test]
    fn a_broken_cost_model_falls_back_to_counting() {
        let dir = tempfile::tempdir().unwrap();
        let mut stage =
            Progress::in_run(dir.path()).weighted_stage("stage 1b", vec![1.0, f64::NAN, 0.0, 2.0]);
        for _ in 0..4 {
            stage.tick();
        }
        stage.close();

        let text = fs::read_to_string(dir.path().join(LOG_PATH)).unwrap();
        for line in text.lines() {
            assert!(!line.contains("NaN") && !line.contains("inf"), "{line}");
        }
        assert!(
            text.lines()
                .any(|l| l.contains("2/4") && l.contains(" 50%")),
            "{text}"
        );
    }

    /// A stage of nothing is a stage that was skipped, and says so rather than
    /// dividing by its total.
    #[test]
    fn an_empty_stage_reports_itself_complete() {
        let dir = tempfile::tempdir().unwrap();
        Progress::in_run(dir.path()).stage("stage 1b", 0).close();
        for line in log_lines(dir.path()) {
            assert!(line.contains("0/0"), "{line}");
            assert!(line.contains("100%"), "{line}");
            assert!(!line.contains("NaN"), "{line}");
        }
    }

    /// A stage that cannot count its work first reports the tally it has
    /// reached, and neither a share nor an estimate: both would need a
    /// denominator that nobody has.
    #[test]
    fn an_unbounded_stage_reports_a_count_and_no_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let mut stage = Progress::in_run(dir.path()).unbounded_stage("scan");
        for _ in 0..1000 {
            stage.tick();
        }
        stage.close();

        let lines = log_lines(dir.path());
        // Opening and closing; the timer has not come round in a test this
        // fast, and the count alone is not a reason to print.
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(lines[0].contains("      0/?"), "{}", lines[0]);
        assert!(lines[1].contains("   1000/?"), "{}", lines[1]);
        assert!(lines[1].ends_with("done"), "{}", lines[1]);
        for line in &lines {
            assert!(line.contains("elapsed"), "{line}");
            assert!(!line.contains('%'), "{line}");
            assert!(!line.contains("eta"), "{line}");
        }
    }

    /// Five per cent of a stage is a share of the work, not a span of time. A
    /// stage of twenty slow conditions would be silent between its steps without
    /// the ceiling, which is the silence this module exists to end.
    #[test]
    fn the_timer_reports_a_stage_the_step_would_leave_silent() {
        let dir = tempfile::tempdir().unwrap();
        let mut stage = Progress::in_run(dir.path()).stage("slow", 100);
        // Two ticks, a step apart from nothing: the step is five, so neither
        // crosses it. Backdating the last line past the interval is the only
        // way to test the ceiling without waiting for it.
        stage.tick();
        stage.last -= REPORT_INTERVAL;
        stage.tick();
        stage.close();

        let lines = log_lines(dir.path());
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[1].contains("      2/100"), "{}", lines[1]);
    }

    /// The mirror is the run's file, and the run stops accepting writes when it
    /// seals its manifest. A stage outliving its run keeps reporting to standard
    /// error and stops writing to the directory.
    #[test]
    fn a_stage_that_outlives_its_run_stops_writing_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let open = Arc::new(AtomicBool::new(true));
        let mut stage = Progress::in_open_run(dir.path(), Arc::clone(&open)).stage("stage 2", 100);
        for _ in 0..10 {
            stage.tick();
        }
        let while_open = log_lines(dir.path()).len();
        assert!(while_open >= 2, "{while_open}");

        open.store(false, Ordering::SeqCst);
        for _ in 0..80 {
            stage.tick();
        }
        stage.close();
        // The mirror stopped where the run sealed it, and the notice about that
        // went to standard error rather than into the file it is about.
        let after = log_lines(dir.path());
        assert_eq!(after.len(), while_open);
        assert!(!after.iter().any(|l| l.contains("outlived")), "{after:#?}");
    }

    /// A second stage over the same run continues the log rather than replacing
    /// it: a subcommand has several stages, and only the last one would survive.
    #[test]
    fn stages_of_one_run_share_one_log() {
        let dir = tempfile::tempdir().unwrap();
        let progress = Progress::in_run(dir.path());
        progress.stage("stage 1", 0).close();
        progress.stage("stage 2", 0).close();
        let lines = log_lines(dir.path());
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert!(lines[0].contains("stage 1"), "{}", lines[0]);
        assert!(lines[2].contains("stage 2"), "{}", lines[2]);
    }

    fn log_lines(dir: &Path) -> Vec<String> {
        fs::read_to_string(dir.join(LOG_PATH))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }
}
