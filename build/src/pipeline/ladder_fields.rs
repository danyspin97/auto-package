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

/// Clears the ladder memo of `state` together: the spec it is keyed on, its
/// history, the maintainer flag and the recap. The recap is read only with the
/// memo it was written for, so every site that clears the memo calls this.
pub(super) fn clear_ladder_memo(state: &mut super::PkgState) {
    state.ladder_spec_hash = None;
    state.ladder_reason = None;
    state.ladder_needs_maintainer = false;
    state.ladder_recap = LadderRecapFields::default();
}
