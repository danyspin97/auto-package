//! Shared types: errors, events, sinks, and run-directory layout.

pub mod env;
pub mod error;
pub mod event;
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

pub use error::{Error, Result};
