//! What a wait for an OBS build shows while the build is still going: the live
//! status line on a terminal, one progress line a minute elsewhere, and the
//! warning for a state that has not changed for too long. The decisions are
//! pure: the caller reads the terminal, the quiet flag and the width, and
//! passes them in.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ap_core::sink::{clear_live, fmt_elapsed, live_line};

use super::BuildStatus;

/// The warning for a state other than `building` (`scheduled`, `blocked`, ...)
/// that has not changed for this long.
pub(super) const NO_PROGRESS_WARNING: Duration = Duration::from_secs(120);

/// A `building` package is warned about only after its status has been the
/// same for this long. A build can run for hours, so only a long silence is
/// worth a warning; it is repeated at the same interval.
pub(super) const BUILDING_STALL_WARNING: Duration = Duration::from_secs(90 * 60);

/// How long a status may stay the same before its warning is due.
pub(super) fn warn_after(status: &BuildStatus) -> Duration {
    match status {
        BuildStatus::Building => BUILDING_STALL_WARNING,
        _ => NO_PROGRESS_WARNING,
    }
}

/// Whether the warning for an unchanged `status` is due, `since_warning` after
/// the last one (or after the status last changed).
pub(super) fn warning_due(status: &BuildStatus, since_warning: Duration) -> bool {
    since_warning >= warn_after(status)
}

/// What the caller found out about the output for this poll.
#[derive(Debug, Clone, Copy)]
pub(super) struct Output {
    /// Stderr is a terminal.
    pub terminal: bool,
    /// The TUI owns the terminal: nothing at all is written.
    pub quiet: bool,
    /// Columns the live line may use.
    pub width: usize,
    /// Whether the live line is coloured.
    pub colour: bool,
    /// No other build is waiting now: two live lines would overwrite each other.
    pub alone: bool,
    /// The wait's first poll, made right after the commit: no live line yet,
    /// so the line starts at the second poll.
    pub first_poll: bool,
}

/// One thing a poll shows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Shown {
    /// Rewrites the live line on the terminal.
    Live(String),
    /// A progress line for the reporter, so it reaches the logs and the TUI.
    Log(String),
}

/// What one poll of a build that is still going shows: `label` is the state
/// (for example `building`) and `waited` the time since the wait began. On a
/// terminal, a waiting build that is alone gets the live line. Otherwise one
/// log line is due each whole minute; `logged_minutes` is the last minute a
/// log line was given for and moves on when one is.
pub(super) fn shown_while_waiting(
    label: &str,
    waited: Duration,
    out: Output,
    logged_minutes: &mut u64,
) -> Option<Shown> {
    if out.quiet {
        return None;
    }
    let state = format!("OBS build: {label}");
    if out.terminal && out.alone && !out.first_poll {
        return Some(Shown::Live(live_line(
            &state, waited, out.width, out.colour,
        )));
    }
    let minutes = waited.as_secs() / 60;
    if minutes == 0 || minutes <= *logged_minutes {
        return None;
    }
    *logged_minutes = minutes;
    Some(Shown::Log(format!(
        "{state}, {}",
        fmt_elapsed(Duration::from_secs(minutes * 60))
    )))
}

/// The waits for OBS builds running now.
static WAITING: AtomicUsize = AtomicUsize::new(0);

/// Counts one wait for as long as it lasts. Dropping it clears the live line,
/// so a finished wait leaves no status behind.
pub(super) struct WaitCount;

impl WaitCount {
    pub(super) fn enter() -> Self {
        WAITING.fetch_add(1, Ordering::SeqCst);
        Self
    }

    /// Whether no other wait is running.
    pub(super) fn alone(&self) -> bool {
        WAITING.load(Ordering::SeqCst) <= 1
    }
}

impl Drop for WaitCount {
    fn drop(&mut self) {
        WAITING.fetch_sub(1, Ordering::SeqCst);
        clear_live();
    }
}
