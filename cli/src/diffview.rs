//! Unified-diff parsing and a styled rendering of it for the TUI.
//!
//! Everything here is a pure function of the diff text (and a width), so it can
//! be tested without a terminal: [`parse`] turns `osc rdiff` / `diff -u` text
//! into [`Diff`] (files, hunks, line numbers), [`word_diff`] finds the changed
//! part of a modified line, and [`render`] turns a [`Diff`] into styled lines
//! in the spirit of `delta`: a rule under each file name, dimmed hunk headers
//! with their context highlighted, tinted added/removed lines with a +/-
//! gutter and old/new line numbers, and word-level emphasis.

use std::ops::Range;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

/// Diff lines read before the rest is dropped with a notice.
pub const MAX_DIFF_LINES: usize = 20_000;
/// Characters kept of one diff line.
pub const MAX_LINE_CHARS: usize = 2_000;
const TAB_STOP: usize = 4;
/// Tokens per side the word diff will compare (it is quadratic).
const MAX_WORD_TOKENS: usize = 300;
mod model;
mod parse;
mod render;
mod search;
mod word;

#[cfg(test)]
mod tests;

pub use model::*;
pub use parse::*;
pub use render::*;
pub use search::*;
pub use word::*;
