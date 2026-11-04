//! The record `auto-package fix` leaves in `run.jsonl` (see [`BugFixEvent`]).

use serde::{Deserialize, Serialize};

use crate::event::StepId;

/// Which record of a fix this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugFixStage {
    /// What the tool read and staged before the turn.
    Input,
    /// The verdict.
    End,
}

/// One record of `auto-package fix`: the input stage says what the tool read
/// and staged, the end stage says how the fix ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BugFixEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepId>,
    pub pkg: String,
    pub stage: BugFixStage,
    /// The references the user gave, in SUSE spelling.
    #[serde(default)]
    pub refs: Vec<String>,
    /// The hosts and files that were read.
    #[serde(default)]
    pub read: Vec<String>,
    /// What was not read, with the reason.
    #[serde(default)]
    pub not_read: Vec<String>,
    /// The fix patch's file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_sha256: Option<String>,
    /// Where the staged patch came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `fixed`, `already_fixed`, `not_affected`, `no_fix`, `needs_maintainer`
    /// or `does_not_apply`; set at the end stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// How the patch went in: `clean`, `refreshed`, `adapted` or `written`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<String>,
    #[serde(default)]
    pub reason: String,
}

impl BugFixEvent {
    /// The line the human log prints.
    pub fn line(&self) -> String {
        match self.stage {
            BugFixStage::Input => format!(
                "  bug fix input: {}{}{}",
                if self.refs.is_empty() {
                    "no reference".to_string()
                } else {
                    self.refs.join(" ")
                },
                self.patch
                    .as_ref()
                    .map(|p| format!(", staged {p}"))
                    .unwrap_or_default(),
                if self.not_read.is_empty() {
                    String::new()
                } else {
                    format!(" ({} not read)", self.not_read.len())
                },
            ),
            BugFixStage::End => format!(
                "  bug fix {}{}: {}",
                self.verdict.as_deref().unwrap_or("ended"),
                self.applied
                    .as_ref()
                    .map(|a| format!(" ({a})"))
                    .unwrap_or_default(),
                self.reason
            ),
        }
    }
}

/// What `auto-package fix --request` did with one finding of the review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingFixEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepId>,
    pub pkg: String,
    /// The request the finding is about.
    pub request: String,
    /// The finding's id.
    pub finding: String,
    /// How the finding was routed: `tool`, `agent`, `pipeline`, `wait`,
    /// `maintainer` or `not_acted_on`.
    pub route: String,
    /// `fixed`, `not_fixed`, `maintainer` or `not_acted_on`.
    pub outcome: String,
    #[serde(default)]
    pub reason: String,
}

impl FindingFixEvent {
    /// The line the human log prints.
    pub fn line(&self) -> String {
        format!(
            "  request {} finding {} ({}): {}{}",
            self.request,
            self.finding,
            self.route,
            self.outcome,
            if self.reason.is_empty() {
                String::new()
            } else {
                format!(": {}", self.reason)
            }
        )
    }
}
