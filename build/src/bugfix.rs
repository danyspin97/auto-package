//! `auto-package fix`: fix one reported bug in a package at the version it has.
//!
//! Everything before the pipeline runs here, in the CLI, before anything is
//! branched: the bug references and text the user gave ([`input`]).
//! The functions are pure except where a module says it reads a file or the
//! network.

mod bugzilla;
#[cfg(test)]
mod bugzilla_tests;
mod cve;
#[cfg(test)]
mod cve_tests;
mod input;
#[cfg(test)]
mod input_tests;
mod lookup;
#[cfg(test)]
mod lookup_tests;
mod patch;
#[cfg(test)]
mod patch_tests;

pub use bugzilla::*;
pub use cve::*;
pub use input::*;
pub use lookup::*;
pub use patch::*;
