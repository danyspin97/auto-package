//! The final result of building one package.

use serde::{Deserialize, Serialize};

/// Final result of building one package, as seen by the CLI's build loop.
/// Unlike per-step events, this is not tied to a `StepId` — it is reported
/// once a package's thread has been joined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackageOutcome {
    Built,
    /// Built on this machine by `--local`: OBS has no build of it.
    BuiltLocally,
    AlreadyBuilt,
    MissingDeps {
        deps: Vec<String>,
    },
    Failed {
        reason: String,
        /// The failure was a full disk, so the user can free space and retry.
        #[serde(default)]
        disk_full: bool,
        /// What must be done before a rerun can succeed, when there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remedy: Option<crate::remedy::Remedy>,
        /// The full detail and the one action beyond `reason`, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<crate::failure_note::FailureNote>,
    },
    /// Stopped for a maintainer's decision (see `FailureKind::NeedsMaintainer`):
    /// nothing was sent to OBS, and it is not a build failure.
    NeedsMaintainer {
        reason: String,
        /// The full detail and the one action beyond `reason`, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<crate::failure_note::FailureNote>,
    },
    /// OBS excludes the package for the build's repository and arch: nothing
    /// was built and nothing is a failure. The reason says why.
    Skipped {
        reason: String,
    },
    /// The user stopped the run while the package was being built: it is not a
    /// failure, and it can be run again (its state reads `interrupted`).
    Interrupted,
}
