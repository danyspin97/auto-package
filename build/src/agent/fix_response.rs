//! A FixBug turn's answer: the verdict on the bug in the Context.

use serde::Deserialize;

/// What a FixBug turn says about the bug.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FixAnswer {
    pub verdict: FixVerdict,
    /// The patch file that carries the fix; for `fixed`.
    #[serde(default)]
    pub patch: String,
    /// The URL of the upstream fix the turn read, or empty.
    #[serde(default)]
    pub upstream: String,
    pub reason: String,
    /// The code that shows the bug is fixed already or does not apply.
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// The verdict of a FixBug turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixVerdict {
    /// A patch carries the fix.
    Fixed,
    /// The source has the fix already.
    AlreadyFixed,
    /// The code the bug is about is not in this version.
    NotAffected,
    /// No fix was found and none could be written.
    NoFix,
    /// A maintainer decides: an ABI or behaviour change, a version update.
    NeedsMaintainer,
}
