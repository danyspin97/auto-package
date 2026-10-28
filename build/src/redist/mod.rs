//! `--nvidia-redist`: CUDA components packaged from NVIDIA's own redistributable
//! archives (`cuda-cublas`, `cuda-cudnn`...) instead of from the `nvidia-*`
//! PyPI wheels (`python-nvidia-cublas`).
//!
//! The mode is one process-wide switch, set once at startup like the disk
//! guard's minimum: the package name of a dependency is derived in a dozen
//! places that have no config at hand ([`crate::osc::rpm_name`] first of all),
//! and with the switch off every one of them behaves as before.
//!
//! A component is named three ways, and [`component`] maps all of them to the
//! manifest key (`libcublas`): the PyPI wheel (`nvidia-cublas-cu13`), a soname
//! (`libcublas.so.13()(64bit)`) and the package this module names it
//! (`cuda-cublas`). The package version is the component's own manifest
//! version (`13.8.0.4`), which is not the wheel's (`13.1.1.3`): the resolver
//! overwrites the node versions with [`apply_manifest_versions`] so every later
//! comparison of "what version is built" uses the one scheme.
//!
//! Network access goes through [`Source`], so the generation can be run on a
//! manifest and an archive the caller supplies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use ap_ecosystem::nvidia_redist::{self, Manifest, Product};

use crate::nvidia_spec::{RedistSpecInput, short_name};

mod components;
mod generate;
mod source;
mod switch;
#[cfg(test)]
pub(crate) mod tests;

pub use components::*;
pub use generate::*;
pub use source::*;
pub use switch::*;
