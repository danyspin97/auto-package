//! Branching, project-meta build paths, binary search and submit requests:
//! the OBS operations behind `update`.
//!
//! Everything that builds an argument list, a query or a document is a pure
//! function with its own tests; the thin wrappers that run `osc` around them
//! need a live OBS and are not covered.

use std::time::Duration;

use crate::proc::{self, Captured, Emit};
mod api;
pub use api::*;
mod create;
pub use create::*;
mod rebase;
pub use rebase::*;
mod project_meta;
pub use project_meta::*;
mod package_flags;
pub use package_flags::*;
pub(crate) use package_flags::{child_indent, layout, open_tags};
mod published;
pub use published::*;
mod provides;
pub use provides::*;
#[cfg(test)]
mod provides_tests;
mod request_read;
pub use request_read::*;
mod submit;
pub use submit::*;
mod supersede;
pub use supersede::*;
mod delete_request;
pub use delete_request::*;
mod origin_health;
pub use origin_health::*;
mod factory_devel;
pub use factory_devel::*;
#[cfg(test)]
mod delete_request_tests;
#[cfg(test)]
mod origin_health_tests;

#[cfg(test)]
mod package_enable_tests;
#[cfg(test)]
mod package_flags_tests;
#[cfg(test)]
mod refusal_tests;
#[cfg(test)]
mod tests;
