//! How a fix ended, kept in the workspace for the CLI to read.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The file in the package's workspace.
pub const FIX_END_FILE: &str = "fix-end.json";

/// The verdict of the fix stage: `fixed`, `already_fixed`, `not_affected`,
/// `no_fix`, `needs_maintainer` or `does_not_apply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixEnd {
    /// The nonce of the run's job: a record of another run is not this one's.
    pub nonce: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// How the patch went in: `clean`, `refreshed`, `adapted` or `written`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<String>,
    #[serde(default)]
    pub reason: String,
}

/// Write the record into the workspace `dir`.
pub fn write_fix_end(dir: &Path, end: &FixEnd) -> Result<(), String> {
    let text = serde_json::to_string_pretty(end).map_err(|e| e.to_string())?;
    ap_core::fsx::write_atomic(&dir.join(FIX_END_FILE), text)
        .map_err(|e| format!("cannot write {FIX_END_FILE}: {e}"))
}

/// The record of the run with `nonce`; `None` when there is none, it does not
/// parse, or it belongs to another run.
pub fn read_fix_end(dir: &Path, nonce: &str) -> Option<FixEnd> {
    let text = std::fs::read_to_string(dir.join(FIX_END_FILE)).ok()?;
    serde_json::from_str::<FixEnd>(&text)
        .ok()
        .filter(|e| e.nonce == nonce)
}
