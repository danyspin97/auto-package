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
mod review_tools;
pub(crate) use review_tools::safe_review_patch_name;
use review_tools::*;
mod edit_bytes;
use edit_bytes::*;
mod integrity;
mod links;
mod owned_files;
mod run;
mod user_prompt;
pub(crate) use user_prompt::*;
mod build_prompt;
mod fix_bug_text;
mod fix_findings_response;
mod fix_findings_text;
mod fix_response;
mod fix_tools;
use fix_bug_text::*;
pub use fix_findings_response::{FindingAnswer, FindingStatus};
use fix_findings_text::*;
pub use fix_response::*;
mod patch_review_text;
use patch_review_text::*;
mod request_review_text;
use request_review_text::*;
mod review_text;
use review_text::*;

#[cfg(test)]
mod tests;
