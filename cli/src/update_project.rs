//! `update`: update the packages of an OBS project that have a newer
//! upstream release, in a branch project, and optionally submit them back.
//!
//! The source project is only ever read (listing, spec fetches for the plan).
//! Packages are branched into the branch project and built there by the
//! ordinary pipeline, exactly as `update` builds in its own project, so the
//! fix loop, notes, budget and locking behave the same.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ap_build::depreach::{self, DepResolver, DepState, DryRunObs, LiveObs};
use ap_build::osc::{self, BranchResult, PathAddition};
use ap_build::proc::Emit;
use ap_build::quality;
use ap_build::update::{self, Ecosystem};
use ap_build::update_project::{
    self as up, Item, ItemOutcome, ItemResult, MissingRequirement, SrOutcome,
};
use ap_core::event::{Event, RunOutcome};
use ap_core::run::RunDir;
use ap_core::sink::{FanoutReporter, HumanSink, JsonlSink, LogLevel, MemorySink, Reporter};
use color_eyre::eyre::eyre;

use super::{
    ProjectLock, Semaphore, Sweep, catch_panic, cmd_build, install_interrupt_handler,
    make_semaphore, print_run_summary, reject_symlink_project_dir, repo_package_name, tui,
};

/// How many packages `--update-deps` may pull in, against a dependency chain
/// that keeps growing.
const MAX_DEPENDENCY_ITEMS: usize = 25;

mod arch;
mod args;
mod audit;
mod cmd;
mod context;
mod deprecated;
mod existing;
mod existing_lint;
mod fix_landed;
#[cfg(test)]
mod fix_landed_tests;
mod items;
mod maintenance;
mod origin;
mod request;
mod requested;
mod rpmlint_audit;
mod run;
mod submit;
mod tidy;

#[cfg(test)]
mod tests;

pub use args::*;
pub use run::*;

use audit::*;
use cmd::*;
use context::*;
use items::*;
use maintenance::*;
use request::*;
use submit::*;
use tidy::*;
