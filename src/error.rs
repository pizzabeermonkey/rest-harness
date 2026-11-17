//! Crate-wide error type for REST Harness.
//!
//! All fallible operations in [`crate::storage`], [`crate::config`] and
//! [`crate::history`] return [`Result`] with this error type.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by REST Harness persistence and configuration code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem failure.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Path the failed operation touched, when known.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// YAML (de)serialization failure.
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// A requested file or directory does not exist.
    #[error("not found: {}", .0.display())]
    NotFound(PathBuf),

    /// A name is empty, dot-only, or otherwise unusable as a filename.
    #[error("invalid name {name:?}: {reason}")]
    InvalidName {
        /// The rejected name.
        name: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A file or directory that must not exist already does.
    #[error("already exists: {}", .0.display())]
    AlreadyExists(PathBuf),

    /// A `{{variable}}` placeholder has no value in the active Environment/Secrets.
    #[error("missing variables: {variables:?}")]
    MissingVariables {
        /// Sorted, deduplicated names of every unresolved variable.
        variables: Vec<String>,
    },

    /// Anything else that does not fit a structured variant.
    #[error("{0}")]
    Other(String),
}

/// Convenience helper for building an [`Error::Io`] with a path attached.
pub(crate) fn io_error(path: &std::path::Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}
