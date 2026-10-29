//! Maintenance requests for SLE and Leap code streams.
//!
//! A package that is maintained in a released code stream (SUSE:SLE-15-SP7:Update,
//! openSUSE:Leap:15.6:Update, ...) is not changed with a submit request to its
//! origin. The change goes to the maintenance project instead, which opens an
//! incident and releases it into the code stream. This is what `osc mbranch` and
//! `osc maintenancerequest` do by hand; here it rides on `update` and `lint`:
//!
//! 1. Which code streams maintain the package: the dry run of the attribute
//!    branch `osc maintained` makes,
//!    `POST /source?cmd=branch&attribute=OBS:Maintained&update_project_attribute=OBS:UpdateProject&dryrun=1&package=PKG`,
//!    which lists each update project with the package.
//! 2. For every chosen code stream, the ordinary `update`/`lint` run with the
//!    update project as its source and `home:<user>:branches:<update project>`
//!    as its branch project (what `osc branch <update project> PKG` names it).
//!    That run branches the package and builds the change there; there is no
//!    second build pipeline. `osc mbranch` would put every code stream into one
//!    `home:<user>:branches:OBS_Maintained:PKG` project with packages renamed
//!    `PKG.<update project>`, which the pipeline cannot build (it needs the
//!    package under its own name, in a project with one build target), so each
//!    code stream gets its own branch project, as `osc branch` makes them.
//! 3. The bug references go into the newest `.changes` entry, the way SUSE
//!    writes them: `- Update to 1.2.3 (bsc#123456, CVE-2025-1234):`.
//! 4. One `maintenance_incident` request per package, with one action per code
//!    stream (source: the branch package; target: the project with the
//!    `OBS:MaintenanceProject` attribute, `releaseproject`: the update project),
//!    created with `POST /request?cmd=create&addrevision=1`, which is the request
//!    `osc maintenancerequest BRANCH PKG UPDATEPROJECT` creates. A code stream
//!    whose branch package already has an open request, or an accepted one of
//!    the same source revision, is left out of it.
//!
//! A run is in one [`Mode`]: `backport` (the default: each code stream keeps
//! its version and gets the upstream fix of `--backport` as a patch, see
//! [`crate::backport`]), `update` (each code stream gets the version `update`
//! builds) or `lint` (packaging fixes). Every code stream is built from its own
//! package and reported on its own ([`StreamReport`]): one that fails is left
//! out of the request with the reason. Which code streams a fix affects can be
//! decided by an external tool through [`StreamCheck`] (`--affected-by`).
//! Code streams maintained in git (SLFO, `[git]` in `osc maintained`) are
//! listed and left out: they are not supported yet.
//!
//! Everything that builds a query, a document or a text is a pure function with
//! its own tests; the wrappers that run `osc` need a live OBS and are not covered.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::backport;
use crate::osc::{self, Error};
use crate::proc::{self, Captured, Emit};

mod bugs;
mod check;
mod codestreams;
#[cfg(test)]
mod helpers;
mod mode;
#[cfg(test)]
mod plan_tests;
mod request;
mod run;
#[cfg(test)]
mod run_tests;
mod select;
mod setup;
#[cfg(test)]
mod undo_refs_tests;

pub use bugs::*;
pub use check::*;
pub use codestreams::*;
pub use mode::*;
pub use request::*;
pub use run::*;
pub use select::*;
pub use setup::*;
