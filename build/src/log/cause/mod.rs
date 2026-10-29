//! A probable cause, read from the build log in Rust.
//!
//! Most failures are one of a few mechanical shapes — a third-party module the
//! tests import that is not a BuildRequires, a header or tool the build cannot
//! find, a CMake/pkg-config package that is absent. Spending an agent turn
//! (reading `build.log`, grepping the sources, working out which line matters)
//! to rediscover that costs time and money and sometimes goes wrong. So the
//! extractor states the cause and the first thing to do about it, with the log
//! lines that justify it, and the prompt leads with that.
//!
//! This is a pure function of the extracted log, the package name and the spec
//! text: no I/O, so every shape is unit-tested.

use std::sync::LazyLock;

use regex::Regex;

use super::{ExtractedLog, PathClass, PythonFrame};

mod classify;
mod detect;
mod known;
#[cfg(test)]
mod tests;

pub use classify::*;
pub use detect::*;
use known::*;
