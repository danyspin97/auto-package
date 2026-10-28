//! The pip dependency resolver.
//!
//! `pip install --dry-run --report -` resolves a package's full runtime
//! closure without installing anything. The report is JSON (PEP 723 report
//! format v1) and contains each node's name, version, sha256, and
//! `requires_dist` with markers unevaluated.
//!
//! Resolution may build sdists, and building runs the sdist's build backend:
//! arbitrary code from the index. So pip runs inside a bubblewrap sandbox
//! (see [`crate::pip_sandbox`]) with a cleared environment that holds only the
//! allowlisted variables (`PIP_ENV_ALLOWLIST`): the backend sees neither the
//! user's files nor OBS, GitHub or Anthropic credentials, and cannot write
//! outside scratch space. The network stays open, as resolution needs it.
//!
//! Without a working sandbox pip is not run on the host. Resolution falls
//! back to wheels only (`--only-binary=:all:`), and [`set_unsafe_resolve`]
//! (`--unsafe-resolve`, or `AUTO_PACKAGE_UNSAFE_RESOLVE=1`) opts into running
//! pip unsandboxed. The same switch turns the sandbox off where it works.
//!
//! Known limitation: `--python-version` is ignored by pip when a real
//! interpreter is running — marker evaluation uses the host interpreter.
//! Cross-flavor resolution (python311 vs python313) requires the actual
//! interpreter binaries to be installed and a separate run per flavor.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pip_sandbox::{self, SandboxError};

/// One package in the resolved closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// Raw `Requires-Dist` lines from the wheel metadata, markers and extras
    /// unevaluated. Empty when the package declares no runtime dependencies.
    pub requires_dist: Vec<String>,
    pub requires_python: Option<String>,
    /// The `requested` flag is true only for the root package — pip sets it to
    /// distinguish what you asked for from what the solver pulled in.
    pub is_requested: bool,
    pub is_yanked: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run pip: {0}")]
    Launch(std::io::Error),

    #[error("package not found on PyPI: {0}")]
    NotFound(String),

    /// Wheels-only resolution (no sandbox, no opt-in) found nothing. The
    /// release may exist as an sdist only.
    #[error(
        "no wheel found for {0}; it may ship only an sdist, which is not built because \
         there is no sandbox to build it in (install bubblewrap and enable unprivileged \
         user namespaces, or pass --unsafe-resolve to build it unsandboxed)"
    )]
    NoWheel(String),

    #[error(transparent)]
    Sandbox(#[from] SandboxError),

    /// Every release exists but pip skipped all of them because their
    /// `Requires-Python` excludes the interpreter pip runs under.
    #[error(
        "{name} is on PyPI, but no release supports the Python that pip runs under \
         (e.g. {example}); put a pip for an older/newer Python first on PATH"
    )]
    IncompatiblePython { name: String, example: String },

    #[error("pip exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("pip produced malformed JSON: {0}")]
    Malformed(#[from] serde_json::Error),

    /// pip resolved the package but couldn't find a sha256 for a node. This
    /// indicates a corrupted or non-standard index, not a usage error.
    #[error("no sha256 for {0}")]
    NoDigest(String),
}

mod cache;
mod mode;
mod probe;
mod resolve;
mod run;

#[cfg(test)]
mod candidate_tests;
#[cfg(test)]
mod classify_failure_tests;
#[cfg(test)]
mod tests;

use cache::*;
pub use mode::*;
use probe::*;
pub use resolve::*;
use run::*;

#[cfg(test)]
mod sandbox_tests;
