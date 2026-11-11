//! The short recap kept in the state next to the memo of a spent ladder. Its
//! fields are flattened into `PkgState`, so state.json keeps one flat object
//! and a file written before they existed still loads.

use serde::{Deserialize, Serialize};

/// What the user is told when the memo refuses a rerun, as `ladder_reason`
/// holds the full history. Read only with `ladder_spec_hash`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderRecapFields {
    /// The short recap: its headline and lines on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder_summary: Option<String>,
    /// The step the recap suggests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder_action: Option<String>,
}
