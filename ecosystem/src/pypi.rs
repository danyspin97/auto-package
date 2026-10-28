//! The PyPI JSON API, and the two things it will not tell us.
//!
//! `GET /pypi/<name>/json` describes the latest release,
//! `GET /pypi/<name>/<version>/json` a pinned one. Both return the same
//! envelope: an `info` object, and a `urls` array of the files uploaded for
//! that release.
//!
//! - **`requires_dist` is frequently `null`.** It is `null` for MarkupSafe
//!   3.0.3, a package that plainly has requirements. Runtime dependencies have
//!   to come from the sdist.
//! - **`[build-system] requires` is not in the response at all.** Whether a
//!   package needs `hatchling`, `flit-core`, `poetry-core` or `setuptools` is
//!   recorded only in the sdist's `pyproject.toml`.
//!
//! So this module's job stops at the sdist: identify the release, and say
//! exactly which tarball to fetch and what it should hash to. Everything else
//! is decided after unpacking.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

mod fetch;
mod model;
mod names;
mod parse;
mod select;
mod version;

#[cfg(test)]
mod tests;

pub use fetch::*;
pub use model::*;
pub use names::*;
pub use parse::*;
pub use select::*;
pub use version::*;
