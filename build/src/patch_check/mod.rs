//! Test a spec's patches against the unpacked new sources with real quilt.
//!
//! A version bump keeps the recipe's patches (see `carry_forward_spec`); some no
//! longer apply to the new release. This module builds the quilt series from
//! the spec's `PatchN:` lines in the order rpm applies them, honouring each
//! patch's `-pN`, pushes it one patch at a time on a scratch copy of the source
//! archive and classifies every patch. The checkout and the workspace's
//! `source/` are never touched: everything happens in a temporary directory.
//!
//! A patch is never classified as removable because it fails: only quilt's
//! own `Patch X can be reverse-applied` (its change is already in the new
//! sources) is [`PatchStatus::AlreadyApplied`], the one proof that removing a
//! patch is right.
//!
//! The parsing of quilt's output, the series and the agent's report are pure
//! functions, tested without a quilt binary.

use std::path::{Path, PathBuf};
use std::time::Duration;

mod applied;
#[cfg(test)]
mod applied_tests;
mod post_image;
#[cfg(test)]
mod post_image_tests;
mod present;
#[cfg(test)]
mod present_tests;
mod quilt;
mod rounds;
mod run;
mod series;
#[cfg(test)]
mod tests;

pub use applied::*;
pub use present::*;
use quilt::*;
pub use rounds::*;
pub use run::*;
pub use series::*;
