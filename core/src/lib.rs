//! Shared types: errors, events, sinks, and run-directory layout.

pub mod error;
pub mod event;
pub mod run;
pub mod sink;

pub use error::{Error, Result};
