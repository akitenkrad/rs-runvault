//! Plain-file experiment tracking for reproducible research.
//!
//! One run is one directory, and the directory is the source of truth. Every
//! layer above it (a DuckDB index, MLflow, a dashboard) reads those files and
//! can be removed without touching the record.
//!
//! ```no_run
//! use runvault::{Run, RunOptions};
//!
//! # fn main() -> runvault::Result<()> {
//! let cfg = serde_json::json!({ "rows": 13, "cols": 16, "seed": 42 });
//! let mut run = Run::start(
//!     RunOptions::new("schelling", "main")
//!         .repo_id("social-simulation-replications")
//!         .domain("simulation")
//!         .parameters(&cfg)?
//!         .seed_pointers(["/seed"])
//!         .master_seed(42),
//! )?;
//! run.log_metric("segregation_index", 0.834).step(120, "step").send()?;
//! run.finish()?;
//! # Ok(())
//! # }
//! ```
//!
//! The shape of every file is frozen by `schema/v1/`, and the rules that decide
//! whether two implementations agree on a hash are pinned by
//! `schema/v1/testvectors/`.

#![warn(missing_docs)]

pub mod canonical;
pub mod config;
pub mod env;
pub mod error;
pub mod files;
pub mod gc;
pub mod git;
pub mod hash;
pub mod ids;
pub mod lockfile;
pub mod meta;
pub mod paths;
pub mod pointer;
pub mod run;
pub mod status;
pub mod verify;
pub mod vocabulary;

pub use error::{Error, Result};
pub use meta::{
    Dataset, Lineage, Llm, Origin, Replication, Research, RunMeta, Target, Visibility, Work,
};
pub use run::{Run, RunOptions};
pub use status::{RunStatus, State};
