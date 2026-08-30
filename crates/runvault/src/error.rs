//! Errors surfaced by `runvault`.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while recording a run.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A value that cannot be canonicalized (`NaN` / `Inf`, or a duplicate key after NFC).
    #[error("正規化できない値です: {0}")]
    Canonical(String),

    /// A JSON Pointer that does not resolve inside `/parameters`.
    #[error("JSON Pointer `{pointer}` は /parameters に存在しません")]
    PointerNotFound {
        /// The pointer as written in `config.json`.
        pointer: String,
    },

    /// A malformed JSON Pointer (does not start with `/`, or has a bad escape).
    #[error("JSON Pointer `{pointer}` の書式が不正です")]
    PointerSyntax {
        /// The pointer as written in `config.json`.
        pointer: String,
    },

    /// An identifier that does not match the slug grammar of `schema/v1/common.json`.
    #[error("`{value}` は {field} の識別子として使えません (^[a-z0-9][a-z0-9._-]{{0,63}}$)")]
    Slug {
        /// Which field rejected the value.
        field: &'static str,
        /// The offending value.
        value: String,
    },

    /// A rule from the design note that the caller broke (missing seed, bad lineage, ...).
    #[error("{0}")]
    Spec(String),

    /// A run directory that contradicts itself (`runvault verify`).
    #[error("{0}")]
    Verify(String),

    /// Filesystem failure, with the path that caused it.
    #[error("{path}: {source}")]
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// Filesystem failure with no useful path to report.
    #[error(transparent)]
    PlainIo(#[from] std::io::Error),

    /// JSON that could not be parsed or serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// CSV that could not be written or parsed.
    #[error(transparent)]
    Csv(#[from] csv::Error),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn spec(msg: impl Into<String>) -> Self {
        Self::Spec(msg.into())
    }

    pub(crate) fn verify(msg: impl Into<String>) -> Self {
        Self::Verify(msg.into())
    }
}
