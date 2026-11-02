//! The other-architecture check of `update`: after the package built on the
//! architecture it was asked for, the same package is checked on every other
//! architecture the origin project builds it for.
//!
//! This module holds the parts that need no OBS: the architecture names and
//! lists, the arch set to check, and the meta edit that adds an architecture to
//! a branch repository. The OBS reads and the orchestration live in the CLI.

mod arches;
mod meta;
mod obs;
#[cfg(test)]
mod tests;

pub use arches::*;
pub use meta::*;
pub use obs::*;

use std::time::Duration;

/// The architectures OBS names in project metas, the only ones `--arches`
/// accepts.
pub const KNOWN_ARCHES: &[&str] = &[
    "x86_64", "aarch64", "armv7l", "armv6l", "i586", "ppc64le", "s390x", "riscv64", "ppc64", "ppc",
];

/// How long the check waits, in total, for the other-architecture builds to
/// finish when `--arch-wait-minutes` is not given.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30 * 60);
