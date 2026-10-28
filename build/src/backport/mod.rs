//! Backports for maintenance runs (`update --maintenance --backport SRC`).
//!
//! A code stream of SLE or Leap normally takes a fix on the version it has, not
//! a version update. The fix is an upstream patch: a file, an https URL of a
//! `.patch`/`.diff` or of a forge commit (whose `.patch` form the forge serves),
//! or a commit id of `--backport-repo`. The run fetches it once ([`prepare`]),
//! puts it into each code stream's branch checkout as `<name>.patch`
//! ([`backport_patches`]) and hands the agent the job of adding it to the spec
//! ([`Job::problem`]); the pipeline's quilt check (`patch_check`) then treats
//! the new `PatchN:` like every other patch of the spec, and refreshes it when
//! it applies with an offset or fuzz. After the build, [`classify`] tells from
//! what the branch holds whether the patch went in as fetched, was refreshed
//! for that code stream's source, or is missing.
//!
//! Everything but [`prepare`], [`fetch`] and [`backport_patches`] is pure.

use std::path::{Path, PathBuf};

use crate::maintenance::is_cve;

mod run;
mod source;
#[cfg(test)]
mod tests;

pub use run::*;
pub use source::*;
