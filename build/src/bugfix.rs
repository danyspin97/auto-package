//! `auto-package fix`: fix one reported bug in a package at the version it has.
//!
//! Everything before the pipeline runs here, in the CLI, before anything is
//! branched: the bug references and text the user gave ([`input`]), a public
//! bug read from Bugzilla ([`bugzilla`]) or a CVE record ([`cve`]), the
//! upstream fix ([`lookup`]) and the name and header of the patch that carries
//! it ([`patch`]), assembled by [`prepare_fix`]. The functions are pure except
//! where a module says it reads a file or the network.

mod bugzilla;
#[cfg(test)]
mod bugzilla_tests;
mod cve;
#[cfg(test)]
mod cve_tests;
mod input;
#[cfg(test)]
mod input_tests;
mod live;
mod lookup;
#[cfg(test)]
mod lookup_tests;
mod patch;
#[cfg(test)]
mod patch_tests;
mod prepare;
#[cfg(test)]
mod prepare_tests;

pub use bugzilla::*;
pub use cve::*;
pub use input::*;
pub use live::*;
pub use lookup::*;
pub use patch::*;
pub use prepare::*;
