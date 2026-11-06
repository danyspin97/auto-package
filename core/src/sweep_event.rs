//! The package an `Event::SweepProgress` is reported for (see [`SweptPackage`]).

use serde::{Deserialize, Serialize};

/// A package the sweep of an `update` has just checked, and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweptPackage {
    /// The name the package is reported under from here on: for an update the
    /// pipeline can build, the name its build reports; else its OBS name.
    pub name: String,
    pub verdict: SweepVerdict,
}

/// What the check of one package found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SweepVerdict {
    /// Upstream has nothing newer than `version`.
    UpToDate { version: String },
    /// Upstream has `to`, newer than the package's `from`.
    Update { from: String, to: String },
    /// The package was not compared, for `reason`.
    Skipped { reason: String },
}
