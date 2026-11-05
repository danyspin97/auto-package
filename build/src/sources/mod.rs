//! User-supplied source repositories for packages that have none on PyPI.
//!
//! A wheel-only package whose metadata names no repository cannot be built at
//! all — there is nothing to put in `Source0`. That is not a rare corner:
//! NVIDIA ships most of its CUDA Python packages as wheels with no
//! `project_urls` at all, and the same is true of plenty of vendor SDKs.
//!
//! The answer is to let the user say where the source lives. The mapping is
//! keyed by package rather than passed as a single flag because the package
//! that needs it is usually a *dependency* discovered mid-run, not the one the
//! user named; and it is persisted to `<work-dir>/sources.toml` so the answer
//! is given once rather than on every invocation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::http::FetchFailure;

mod archive;
#[cfg(test)]
mod fetch_tests;
mod forge;
#[cfg(test)]
mod forge_urls;
#[cfg(test)]
mod helpers;
#[cfg(test)]
mod index_tests;
mod kind;
mod map;
mod redirects;
mod release_date;
mod release_list;
mod release_series;
#[cfg(test)]
mod release_tag_tests;
mod releases;
mod sourcehut;
mod tag_list;
use tag_list::*;

pub use archive::*;
pub use forge::*;
pub use kind::*;
pub use map::*;
pub use release_date::*;
pub use release_list::*;
pub use release_series::*;
pub use releases::*;
pub(crate) use sourcehut::{is_sourcehut_host, ls_remote_tags, newest_version};
