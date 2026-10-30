use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use owo_colors::{OwoColorize, Stream::Stderr, Style};
use serde::Serialize;

use crate::event::{Event, HostScope, HostSource, PackageOutcome, RunOutcome, StepOutcome, Usage};

/// Set while a full-screen UI (the TUI) owns the terminal. Any diagnostic
/// output that isn't routed through a `Reporter` — ad hoc `eprintln!`s
/// scattered through resolution/search code — must check this before
/// writing, or it lands on top of the alternate screen and corrupts it.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// `eprintln!`, suppressed while [`set_quiet`] is in effect. Use for ad hoc
/// diagnostic lines that aren't structured `Event`s and so can't reach the
/// TUI as anything else — the alternative is losing them entirely while the
/// TUI is active.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        if !$crate::sink::is_quiet() {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("{}\n", $crate::sink::printable(&format!($($arg)*))),
            );
        }
    };
}

mod human;
mod jsonl;
mod memory;
mod reporter;
mod text;

#[cfg(test)]
mod elapsed_tests;
#[cfg(test)]
mod printable_tests;
#[cfg(test)]
mod tests;

pub use human::*;
pub use jsonl::*;
pub use memory::*;
pub use reporter::*;
pub use text::*;
