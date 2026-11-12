//! The live status line: one line that a long wait rewrites in place on a
//! terminal, e.g. `  ▸ OBS build: building… 2m09s`. This module holds its
//! text; the writes that keep it from clashing with other output are below.

use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use super::fmt_elapsed;
use super::style::{Role, paint};
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
