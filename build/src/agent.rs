//! AI-assisted spec repair — tool-using turn loop only.
//!
//! The only backend is the `claude` CLI (`--agent-backend cli`). Vertex AI and
//! direct-API backends are not supported.

use std::collections::BTreeMap;
use std::process::Command;

use serde::Deserialize;

use crate::log::ExtractedLog;
mod env;
pub(crate) use env::*;
mod turn;
pub use turn::*;
mod prompt;
use prompt::*;
mod outcome;
pub use outcome::*;
mod response;
pub use response::*;
mod tools;
pub(crate) use tools::REFRESH_SERIES;
use tools::*;
mod edit_bytes;
use edit_bytes::*;
mod integrity;
mod links;
mod run;
mod user_prompt;
pub(crate) use user_prompt::*;
mod build_prompt;
mod review_text;
use review_text::*;

#[cfg(test)]
mod tests;
