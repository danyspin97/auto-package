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

use ap_build::arch_check::{ArchDeadline, submit_deadline};
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
mod build_work;
mod cmd;
mod context;
mod dep_listing;
#[cfg(test)]
mod dep_listing_tests;
mod deprecated;
mod deps;
#[cfg(test)]
mod deps_tests;
mod dry_run;
mod routes;
use routes::*;
pub(crate) mod existing;
mod existing_lint;
mod fix_landed;
#[cfg(test)]
mod fix_landed_tests;
#[cfg(test)]
mod gate_ports_tests;
mod incomplete;
mod items;
mod leftovers;
mod maintenance;
mod origin;
mod parked;
#[cfg(test)]
mod parked_tests;
mod patch_headers;
mod provenance;
mod ready;
#[cfg(test)]
mod ready_inherited_tests;
mod ready_scope;
mod request;
mod requested;
mod rpmlint_audit;
mod run;
mod same_project;
mod same_project_defer;
#[cfg(test)]
mod same_project_defer_tests;
#[cfg(test)]
mod same_project_tests;
mod same_wait;
mod schedule;
#[cfg(test)]
mod schedule_tests;
mod submit;
#[cfg(test)]
mod submit_backend_tests;
#[cfg(test)]
mod submit_wait_tests;
mod throttle;
mod tidy;
mod tui_row;
mod unbuilt_waits;
mod unrecognised;

#[cfg(test)]
mod tests;
use incomplete::{none_checked, obs_unavailable};

pub use args::*;
pub(crate) use ready_scope::for_resubmit;
pub(crate) use request::{ArchWait, prepare_request};
pub use run::*;
pub(crate) use submit::{review_gate, yes_refusal};
pub(crate) use tidy::{ask_line, confirm_with};
pub(crate) use tui_row::{queue_found, row_name};

use audit::*;
use build_work::*;
use cmd::*;
use context::*;
use dep_listing::*;
use deps::*;
use dry_run::*;
use items::*;
use maintenance::*;
use request::*;
use schedule::Scheduler;
use submit::*;
use throttle::Throttle;
use tidy::*;
