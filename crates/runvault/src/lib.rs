//! Plain-file experiment tracking for reproducible research.
//!
//! One run is one directory, and the directory is the source of truth. Every
//! layer above it (a DuckDB index, MLflow, a dashboard) reads those files and
//! can be removed without touching the record.

#![warn(missing_docs)]

pub mod canonical;
pub mod config;
pub mod error;
pub mod hash;
pub mod ids;
pub mod meta;
pub mod pointer;
pub mod status;

pub use error::{Error, Result};
