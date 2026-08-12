//! Shared diagnostics and process exit-code conventions for Roller.

use std::path::PathBuf;

use thiserror::Error;

/// Errors produced while loading a Roller source file.
#[derive(Debug, Error)]
pub enum SourceError {
    /// The script could not be read from disk.
    #[error("failed to read Roller script `{path}`: {source}")]
    Read {
        /// Script path requested by the user.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}
