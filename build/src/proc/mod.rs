//! Subprocess execution with captured output and event emission.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ap_core::event::{Event, StepId};
use ap_core::sink::{NULL_REPORTER, Reporter};

mod capture;
#[cfg(test)]
mod capture_timeout_tests;
mod logged;
#[cfg(test)]
mod logged_tests;
mod osc_lock;
#[cfg(test)]
mod osc_lock_tests;
#[cfg(test)]
mod restore_spawn_tests;
mod shutdown;
#[cfg(test)]
mod write_log_tests;

pub use capture::*;
pub use logged::*;
pub use shutdown::*;
