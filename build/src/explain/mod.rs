//! `auto-package explain <pkg>`: what happened to one package in a work
//! directory, read back from the files a run leaves behind. No model, no
//! network, no writes: the report is a deterministic function of
//! `state/<pkg>/`, `runs/<run>/run.jsonl`, `runs/<run>/<pkg>/NNN-<step>/`, the
//! package notes and the checkout.
//!
//! [`explain`] collects an [`Explanation`] (serialisable: `--json`), and
//! [`render`] turns it into styled lines for a given width, which the command
//! prints with ANSI colours and the TUI shows in a modal.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

mod collect;
mod detail;
#[cfg(test)]
mod detail_tests;
mod logs;
mod model;
mod parse;
mod render;
#[cfg(test)]
mod tests;
mod tries;
#[cfg(test)]
mod tries_tests;
mod tries_view;

pub use collect::*;
use logs::*;
pub use model::*;
use parse::*;
pub use render::*;
pub use tries::*;
pub use tries_view::*;
