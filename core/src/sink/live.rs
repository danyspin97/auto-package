//! The live status line: one line that a long wait rewrites in place on a
//! terminal, e.g. `  ▸ OBS build: building… 2m09s`. This module holds its
//! text; the writes that keep it from clashing with other output are below.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use super::style::{Role, paint};
use super::{fmt_elapsed, is_quiet};
use crate::remedy::printable;

/// The columns assumed when the terminal size is not known.
const UNKNOWN_WIDTH: usize = 80;
/// The narrowest line the live status is fitted to.
const MIN_LIVE_WIDTH: usize = 10;

/// The columns the live line is fitted to, given the terminal's real width
/// (`None` when it cannot be read). Not the clamped width of the report
/// layout: a narrow terminal must not make the line wrap.
pub fn live_width(raw: Option<usize>) -> usize {
    raw.filter(|&cols| cols > 0)
        .unwrap_or(UNKNOWN_WIDTH)
        .max(MIN_LIVE_WIDTH)
}

/// Whether the live line is on the terminal now. Every write to stderr that
/// goes through this module holds the lock, so a line never lands inside one.
static SHOWN: Mutex<bool> = Mutex::new(false);

/// Whether a question is waiting for an answer on the terminal (see
/// [`PromptGuard`]). While it is, the live line is not drawn over it.
static PROMPTING: AtomicBool = AtomicBool::new(false);

fn shown() -> std::sync::MutexGuard<'static, bool> {
    SHOWN.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether the live line may be drawn: stderr is a terminal, and `TERM` names
/// one that can move the cursor (set, not empty, not `dumb`). Pure, so the
/// environment is passed in.
pub fn can_draw_live(is_tty: bool, term: Option<&str>) -> bool {
    is_tty && term.is_some_and(|t| !t.is_empty() && t != "dumb")
}

/// [`can_draw_live`] for this process: the real stderr and `TERM`.
pub fn live_capable() -> bool {
    can_draw_live(
        std::io::stderr().is_terminal(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// The terminal's width in columns, when it can be read.
pub fn terminal_columns() -> Option<usize> {
    crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| usize::from(cols))
}

/// Draws `line` as the live line: it replaces the live line shown before, and
/// it has no newline, so the next line written goes on the same row. Nothing
/// is written while the TUI owns the terminal or a question is open.
pub fn show_live(line: &str) {
    if is_quiet() || PROMPTING.load(Ordering::SeqCst) {
        return;
    }
    let mut shown = shown();
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[2K{line}");
    let _ = err.flush();
    *shown = true;
}

/// Writes `line` and its newline to stderr. A live line on the terminal is
/// cleared first, so the line is not left stuck to the end of this one.
pub fn print_line(line: &str) {
    let mut shown = shown();
    let mut err = std::io::stderr().lock();
    if *shown {
        let _ = write!(err, "\r\x1b[2K");
        *shown = false;
    }
    let _ = writeln!(err, "{line}");
}

/// Removes the live line from the terminal, if one is shown.
pub fn clear_live() {
    let mut shown = shown();
    if *shown {
        let _ = write!(std::io::stderr().lock(), "\r\x1b[2K");
        *shown = false;
    }
}

/// Holds the terminal for a question that is answered on the same row: the
/// live line is cleared and not drawn again until the guard drops, so it
/// cannot overwrite the question or the answer.
pub struct PromptGuard;

impl PromptGuard {
    pub fn open() -> Self {
        PROMPTING.store(true, Ordering::SeqCst);
        clear_live();
        Self
    }
}

impl Drop for PromptGuard {
    fn drop(&mut self) {
        PROMPTING.store(false, Ordering::SeqCst);
    }
}

/// The live line for `label` (the state being waited on) after `elapsed`,
/// fitted to `width` columns. Dimmed when `colour` is on. Control characters
/// in the label (ESC, newlines) become `?`, so upstream text cannot move the
/// cursor or break the line. The label is cut with `…` when the line would
/// not fit, by display width, and one column is kept free so a full line does
/// not wrap, which would break the rewrite. With colour off the text has no
/// escape sequences.
pub fn live_line(label: &str, elapsed: Duration, width: usize, colour: bool) -> String {
    let head = format!("  ▸ {}", printable(label));
    let line = fit(&head, &fmt_elapsed(elapsed), width.max(MIN_LIVE_WIDTH));
    paint(&line, Role::Dim, colour)
}

/// `head`, then `…`, then `tail`, with `head` shortened as needed so that the
/// whole fits in `width - 1` columns.
fn fit(head: &str, tail: &str, width: usize) -> String {
    let limit = width.saturating_sub(1);
    let full = format!("{head}… {tail}");
    if UnicodeWidthStr::width(full.as_str()) <= limit {
        return full;
    }
    let tail_width = UnicodeWidthStr::width(tail);
    let mut kept = String::new();
    for c in head.chars() {
        let mut next = kept.clone();
        next.push(c);
        if UnicodeWidthStr::width(next.as_str()) + 2 + tail_width > limit {
            break;
        }
        kept = next;
    }
    format!("{kept}… {tail}")
}
