//! The data and the text of `update`: which packages to update, what a
//! dry run prints, and the end-of-run summary.
//!
//! The OBS and pipeline work that produces these lives in the CLI; this module
//! has none of it, so the plan and report formats are testable on their own.

use crate::fix_flags::FixFlag;
use crate::osc::{self, PathAddition};
use crate::risk_flags::RiskFlag;
use crate::update::{Candidate, Decision, Ecosystem};

mod branch_state;
mod plan;
mod report;
mod requirements;
#[cfg(test)]
mod tests;

pub use branch_state::*;
pub use plan::*;
pub use report::*;
pub use requirements::*;
