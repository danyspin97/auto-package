//! Finding an upstream source repository for a package name, beyond GitHub.
//!
//! A bare name that PyPI does not carry (a C/C++ library, a project hosted on
//! GitLab or Codeberg, a distro-only name) used to be looked up on GitHub
//! alone. This module adds the other places an upstream is likely to live:
//!
//! * **The distribution's own spec** (`openSUSE:Factory`, read with `osc cat`)
//!   — its `URL:` and `Source:` tags are what openSUSE already records as the
//!   project's upstream, which settles name collisions (an unrelated
//!   `lzip` binding on GitHub vs. the real one on Savannah) and reveals
//!   non-forge release directories.
//! * **GitLab.com** and **Codeberg** (Gitea API) project search.
//!
//! Every network call is a thin wrapper around a pure parser, so the
//! response handling is tested against fixtures.

use std::collections::HashMap;

use serde_json::Value;

// ── last resort: ask the agent ─────────────────────────────────────────────────

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;

mod forges;
mod lookup;
#[cfg(test)]
mod tests;

pub use forges::*;
pub use lookup::*;
