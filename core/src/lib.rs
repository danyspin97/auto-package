//! Shared types: errors, events, sinks, and run-directory layout.

pub mod error;
pub mod event;
pub mod proc_lock;
pub mod run;
pub mod sink;
pub mod skills;
pub mod step;

pub use error::{Error, Result};
