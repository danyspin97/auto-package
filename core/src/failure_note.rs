use crate::event::PackageOutcome;
use serde::{Deserialize, Serialize};

/// What a failed package says beyond its short `reason`. The `reason` is the
/// text the user reads; this keeps the rest, so the full text stays in
/// run.jsonl and `explain` while the console shows the short one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureNote {
    /// The full text of the failure, every round included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// One concrete step the user can take (a command or an instruction),
    /// when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl PackageOutcome {
    /// A failure with only its reason: no disk-full flag, remedy or note.
    pub fn failed(reason: String) -> Self {
        PackageOutcome::Failed {
            reason,
            disk_full: false,
            remedy: None,
            note: None,
        }
    }
}
