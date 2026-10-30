//! The full-screen diff viewer and the submit confirmation of the TUI.
//!
//! Both are modals owned by the event loop (`BrowseState::modal`). The slow
//! parts (`osc rdiff`, the checks before a submit request, the request itself)
//! run on background threads behind a [`PackageBackend`] and come back as
//! [`Job`]s, so the UI stays responsive. Every decision is a function of
//! plain state, so it can be tested with a fake backend and no terminal.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ratatui::widgets::Clear;

use super::*;
use crate::diffview::{self, Diff, Rendered};
use ap_build::explain::{self, Explanation};

pub enum Modal {
    Diff(DiffView),
    Submit(SubmitModal),
    Explain(ExplainView),
    AllowHost(allow_host::AllowHostModal),
}
mod allow_host;
mod diff;
mod explain_view;
mod jobs;
mod render;
mod submit;

#[cfg(test)]
mod tests;

pub use allow_host::*;
pub use diff::*;
pub use explain_view::*;
pub use jobs::*;
pub use render::*;
pub use submit::*;
