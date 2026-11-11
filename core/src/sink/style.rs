//! The shared palette for the terminal output: one [`Role`] per kind of text,
//! its SGR codes, and the helpers the formatters share ([`paint`], [`pad_to`],
//! [`terminal_width`]). Whether colour is on is decided by the caller, with
//! [`stderr_colour`](super::stderr_colour) or [`stdout_colour`]. With colour
//! off, [`paint`] returns the text unchanged.
//!
//! A span closes with the code that undoes only its own attribute, never a
//! full reset. Bold and dim both close with 22 and every colour closes with
//! 39, so a Dim span inside a Failure span also ends the bold, and a colour
//! span inside another colour span drops the outer colour. Later formatters
//! must not nest those pairs, or must close the outer span themselves.
//! Padding is measured on the text as given: pad first, then paint.

use owo_colors::{OwoColorize, Stream::Stdout};
use unicode_width::UnicodeWidthStr;

/// The kind of text a span holds. Each role has one fixed SGR style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Success,
    Failure,
    Warning,
    Info,
    Dim,
    Emphasis,
    Url,
    Package,
    VersionOld,
    VersionNew,
    Duration,
    Cost,
    Heading,
}

impl Role {
    /// The SGR parameters that start the role and the ones that end it.
    fn codes(self) -> (&'static str, &'static str) {
        match self {
            Role::Success => ("32", "39"),
            Role::Failure => ("1;31", "22;39"),
            Role::Warning => ("33", "39"),
            Role::Info => ("36", "39"),
            Role::Dim => ("2", "22"),
            Role::Emphasis => ("1", "22"),
            Role::Url => ("4", "24"),
            Role::Package => ("1", "22"),
            Role::VersionOld => ("2", "22"),
            Role::VersionNew => ("32", "39"),
            Role::Duration => ("2", "22"),
            Role::Cost => ("2", "22"),
            Role::Heading => ("1", "22"),
        }
    }
}

/// `text` in the style of `role` when `on` is set, and `text` unchanged when
/// it is not. Opens with the role's code and closes with its own off code
/// (see the module docs for what nesting does to the outer span).
pub fn paint(text: &str, role: Role, on: bool) -> String {
    if !on {
        return text.to_string();
    }
    let (open, close) = role.codes();
    format!("\x1b[{open}m{text}\x1b[{close}m")
}

/// Whether stdout takes colour: a terminal, `NO_COLOR` unset, and
/// `CLICOLOR_FORCE` honoured. The counterpart of [`stderr_colour`](super::stderr_colour).
pub fn stdout_colour() -> bool {
    !"".if_supports_color(Stdout, |t| t.red())
        .to_string()
        .is_empty()
}

/// `text` followed by spaces up to `width` display columns (CJK characters
/// count two, combining marks none). Text already wider is returned whole.
/// The width is measured on `text` as given, escapes included, so pad before
/// painting: `paint(&pad_to(text, w), role, on)` aligns, `pad_to(&paint(..))`
/// does not.
pub fn pad_to(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(UnicodeWidthStr::width(text));
    let mut out = String::with_capacity(text.len() + fill);
    out.push_str(text);
    out.extend(std::iter::repeat_n(' ', fill));
    out
}

const DEFAULT_WIDTH: usize = 100;
const MIN_WIDTH: usize = 40;
const MAX_WIDTH: usize = 140;

/// The width in columns to lay output out to: the terminal's width clamped to
/// 40..=140 when `on_terminal` is set and the size can be read, else 100.
/// The size comes from crossterm, which reads `/dev/tty` and falls back to
/// stdout only when `/dev/tty` cannot be opened. So with stdout piped to a
/// file, the width is still the terminal's, and it can differ from stdout.
pub fn terminal_width(on_terminal: bool) -> usize {
    let raw = if on_terminal {
        crossterm::terminal::size()
            .ok()
            .map(|(cols, _)| usize::from(cols))
    } else {
        None
    };
    width_from_size(raw)
}

/// [`terminal_width`] for a raw column count (`None` when there is none). A
/// count of 0 is no size either, and gives the default.
pub(super) fn width_from_size(raw: Option<usize>) -> usize {
    raw.filter(|&cols| cols > 0)
        .map_or(DEFAULT_WIDTH, |cols| cols.clamp(MIN_WIDTH, MAX_WIDTH))
}
