//! What a package's change adds that a reviewer must look at before it is
//! submitted: a scriptlet the baseline did not have, a source host it did not
//! name, shell or Lua expansion, a changed `Requires(pre/post...)` or
//! `Provides` line, and a patch that touches a file neither the baseline's
//! patches nor the source archive have.
//!
//! Like the fix-loop flags (`fix_flags`), these are read from the diff of the
//! OBS baseline against the final files, never from what an agent said. They
//! are flags, not refusals: `--yes` is what refuses to submit them. A part of
//! the change that cannot be read (a patch name that is not a plain file name
//! in the checkout) is flagged as not analysed, never skipped.
//!
//! The baseline is the spec OBS holds. A package OBS does not hold has no
//! baseline, so every line counts as new for it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::deselect::is_section;
use crate::fix_flags::{extra, one_line};
use crate::pipeline::PkgState;
use crate::workspace::Workspace;

mod analysis;
mod fix;
#[cfg(test)]
mod fix_tests;
mod flags;
#[cfg(test)]
mod tests;

pub use analysis::*;
pub use fix::*;
pub use flags::*;
