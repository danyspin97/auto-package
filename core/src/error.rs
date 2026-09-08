//! Library errors are typed; the binary uses `eyre` for reporting.
//!
//! The distinction matters for one reason: the fix loop *branches* on failure
//! kind. A build that failed because `osc` is missing must not be handed to a
//! model as if it were a spec defect.

use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The build ran and rpmbuild reported failure. This is the *expected*
    /// failure — the one the fix loop exists to handle.
    #[error("build failed at {section} (exit {exit_code})")]
    BuildFailed {
        section: String,
        exit_code: i32,
        log_path: PathBuf,
    },

    /// The build could not be attempted: osc missing, no credentials, disk
    /// full. Never send these to a model; they are the operator's problem.
    #[error("build environment unusable: {0}")]
    BuildEnvironment(String),

    #[error(transparent)]
    Other(#[from] eyre::Report),
}

impl Error {
    /// Whether a failure is worth spending a model call on.
    ///
    /// Run 1 spent an hour on `ENOSPC` masquerading as a build failure; a loop
    /// that retries those burns the budget without ever making progress.
    pub fn is_actionable_by_model(&self) -> bool {
        matches!(self, Error::BuildFailed { .. })
    }
}
