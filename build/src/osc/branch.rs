//! Branching, project-meta build paths, binary search and submit requests:
//! the OBS operations behind `update`.
//!
//! Everything that builds an argument list, a query or a document is a pure
//! function with its own tests; the thin wrappers that run `osc` around them
//! need a live OBS and are not covered.

use std::time::Duration;

use super::{
    Error, OSC_QUERY_TIMEOUT, anywhere, check_obs_name, during, enc, first_line, is_not_found,
    is_valid_obs_name, parse_project_repo_paths,
};
use crate::proc::{self, Captured, Emit};
mod api;
pub use api::*;
mod create;
pub use create::*;
mod rebase;
pub use rebase::*;
mod project_meta;
pub use project_meta::*;
mod published;
pub use published::*;
mod submit;
pub use submit::*;

#[cfg(test)]
mod tests;
