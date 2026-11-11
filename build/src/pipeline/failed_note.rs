//! A failure that carries a [`FailureNote`].

use super::*;
use ap_core::failure_note::FailureNote;

impl RunCtx<'_> {
    /// A failure of `kind` whose `reason` is the short text the user reads and
    /// whose `note` keeps the rest (the full detail, a suggested action).
    // Used by the ladder end, which a later change wires in.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn failed_note(
        &self,
        kind: FailureKind,
        reason: String,
        note: FailureNote,
    ) -> PipelineResult {
        PipelineResult::Failed {
            rpm_name: self.rpm_name.clone(),
            reason,
            kind,
            remedy: None,
            note: Some(note),
        }
    }
}
