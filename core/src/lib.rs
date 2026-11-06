//! Shared types: errors, events, sinks, and run-directory layout.

pub mod build_info;
pub mod env;
pub mod error;
pub mod event;
#[cfg(test)]
mod event_tests;
pub mod fix_event;
pub mod fsx;
pub mod ledger;
pub mod limits;
pub mod pgroup;
pub mod proc_lock;
pub mod redact;
pub mod run;
pub mod sandbox;
pub mod sink;
pub mod skills;
pub mod step;
pub mod sweep_event;

pub use error::{Error, Result};
