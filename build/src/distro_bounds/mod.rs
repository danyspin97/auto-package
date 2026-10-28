//! Does the version a linked project (openSUSE:Factory) ships meet the bound a
//! package that has to be built here declares on it?
//!
//! `select_target` counts a package present in a linked project as resolved
//! without looking at its version. That is right until a package built here
//! declares a bound the shipped version misses: `tilelang` needs
//! `apache-tvm-ffi <=0.1.11` while Factory ships `0.1.14.post1`, `open-webui`
//! pins `redis==8.0.1` against Factory's `7.0.1`. OBS then fails the dependent
//! with `nothing provides ...` and the failure reaches the agent as a missing
//! package, long after the cause was knowable from the closure.
//!
//! The two directions need different handling:
//!
//! - The distro's version is too NEW (above an upper bound or an exact pin):
//!   upstream's bound is usually only the version it was tested with, so the
//!   dependent's spec relaxes it (convention 59), and only a real API break
//!   makes the distro package worth branching and patching (convention 62).
//!   The dependency stays resolved; the conflict is reported.
//! - The distro's version is too OLD (below a lower bound or an exact pin):
//!   relaxing cannot help, so the distro's package is branched into this
//!   project and updated to the version pip resolved, carrying the distro's own
//!   recipe forward (not packaged from scratch), where it shadows the distro's
//!   package. The dependency is no longer resolved.
//!
//! Only dependents that still have to be built here are checked: a package the
//! distro ships was built against what the distro ships.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use ap_ecosystem::pip::Node;
use pep440_rs::{Operator, Version, VersionSpecifiers};

use crate::osc::fuzzy_key;

#[cfg(test)]
mod bound_violation_tests;
mod check;
mod plan;
#[cfg(test)]
mod redist_tests;
#[cfg(test)]
mod tests;

pub use check::*;
pub use plan::*;
