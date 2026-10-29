//! What a package's fix loop changed that its readers must be told: tests
//! switched off, `%check` removed, an upstream wheel put in for a source build,
//! and a turn that answered `blocked`.
//!
//! The spec changes are read from the diff of the OBS baseline against the
//! final spec, so they do not depend on what the agent said it did. A `blocked`
//! answer is recorded by the pipeline when the turn gives it. The flags live in
//! the package's state (`PkgState::fix_flags`) and are rendered by the caller:
//! the `.changes` entry, the submit request and the end-of-run summary.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::deselect::is_section;
use crate::pipeline::PkgState;
use crate::workspace::Workspace;

mod flags;
mod spec_diff;
#[cfg(test)]
mod tests;

pub use flags::*;
pub use spec_diff::*;
