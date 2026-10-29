//! Interactive TUI for monitoring a build run across all packages at once.
//!
//! The build loop spawns one thread per package and they all share a single
//! `Reporter`. Per-step events (`StepStarted`, `Command`, `AgentToolUse`, ...)
//! carry a `StepId` that is only unique *within* a package — two packages
//! building concurrently both start at `StepId(0)`. So attribution to a
//! package cannot be recovered from the event alone; it has to be attached
//! at the point where the package name is known. `TuiController::tap` does
//! that: the CLI asks for a tapped reporter per package thread (and one
//! "global" tap for events with no package, like `RunStarted`), and the tap
//! forwards a `(pkg, event)` pair into the render thread's channel.

use std::collections::{HashMap, VecDeque};
use std::io::{self, IsTerminal};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event as TermEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Wrap,
    },
};

use ap_core::event::{Event, PackageOutcome, RunOutcome, SkipReason, StepKind, StepOutcome, Task};
use ap_core::sink::{MemorySink, Reporter};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod modal;
use modal::{BackendSlot, Job, Modal, SrMark};
pub use modal::{PackageBackend, SubmitFailure, SubmitPreview, set_explain_dir};

const MAX_LOG_LINES: usize = 300;
const MAX_AGENT_ACTIVITY: usize = 100;

/// Bound on events queued for the render thread; beyond it events are dropped.
const EVENT_QUEUE_CAP: usize = 8192;

mod browse;
mod controller;
mod detail;
mod panes;
mod pkg;
mod render;
mod reporter;
mod runloop;
mod screen;
mod state;
mod terminal;
mod text;

pub use controller::*;
pub use reporter::*;
pub use screen::*;
pub use terminal::*;

use browse::*;
use detail::*;
use panes::*;
use pkg::*;
use render::*;
use runloop::*;
use state::*;
use text::*;

#[cfg(test)]
mod browse_tests;
#[cfg(test)]
mod expand_tests;
#[cfg(test)]
mod filter_esc_tests;
#[cfg(test)]
mod header_tests;
#[cfg(test)]
mod prompt_tests;
#[cfg(test)]
mod render_death_tests;
#[cfg(test)]
mod render_smoke_tests;
#[cfg(test)]
mod replay;
#[cfg(test)]
mod row_names_tests;
#[cfg(test)]
mod run_end_tests;
#[cfg(test)]
mod seq_tests;
#[cfg(test)]
mod step_state_tests;
#[cfg(test)]
mod terminal_guard_tests;
#[cfg(test)]
mod truncate_tests;
