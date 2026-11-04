//! `auto-package fix`: fix one reported bug in a package at the version it has.
//!
//! Everything before the pipeline runs here, in the CLI, before anything is
//! branched: the bug references and text the user gave ([`input`]).
//! The functions are pure except where a module says it reads a file or the
//! network.

mod input;
#[cfg(test)]
mod input_tests;

pub use input::*;
