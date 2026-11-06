//! Detection of packages in an OBS project that have a newer upstream release.
//!
//! The sweep is deliberately read-only: it only identifies *which* packages
//! would be bumped and from what to what. Actually bumping them is the job of
//! the ordinary build pipeline — the update command invokes that per
//! candidate once this module has returned the list.
//!
//! The classifier tries several ecosystems in order against each spec's
//! `Source:` line:
//!
//! 1. A direct PyPI sdist URL (`files.pythonhosted.org/packages/source/…`).
//! 2. A PEP 427 wheel filename, as openSUSE's generator uses for packages
//!    PyPI ships as wheels only (NVIDIA's CUDA toolkit components).
//! 3. A bare `<name>-%{version}.tar.gz` filename, treated speculatively as
//!    a PyPI project name (a 404 on PyPI then becomes a per-package skip, not
//!    a hard failure).
//! 4. A git-forge URL (GitHub/GitLab/Gitea), classified via
//!    [`crate::sources::latest_release`].
//!
//! Everything that doesn't match any of these is listed as
//! `SkipReason::NotRecognized` so the user still sees it in the plan output.

use crate::osc;
use crate::proc::Emit;
use crate::spec;
use crate::upstream_cache;
use crate::version::{self, Scheme};
use ap_core::event::Event;
use ap_ecosystem::pypi;
use std::collections::HashMap;
use std::sync::Mutex;

mod candidates;
mod check_pool;
mod host_gate;
pub use check_pool::set_check_jobs;
#[cfg(test)]
mod helpers;
#[cfg(test)]
mod lookup_macro_tests;
#[cfg(test)]
mod lookups;
mod pins;
mod requested;
mod siblings;
mod spellings;
pub use requested::{AlreadyRequested, clean_version};
mod plan;
mod planned_specs;
pub use planned_specs::*;
mod read;
#[cfg(test)]
mod reading;
mod repology;
mod snapshot;
#[cfg(test)]
mod specs;
mod suse;
pub use snapshot::cached_release_date;
#[cfg(test)]
mod snapshot_tests;
pub use suse::project_has_service;
#[cfg(test)]
mod script_version_tests;
#[cfg(test)]
mod suse_tests;
#[cfg(test)]
mod versions;

pub use candidates::*;
mod skip;
use pins::*;
pub use plan::*;
pub use read::*;
pub use skip::SkipReason;
