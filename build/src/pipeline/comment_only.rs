//! An edit made after a green build that changes only the comments of the spec.
//!
//! Every change to the spec is built again on OBS, and a build costs minutes
//! for text rpm never sends to the package. So a turn that runs after the
//! build and changes nothing but the spec's comment lines is discarded: the
//! checkout goes back to the snapshot taken before the turn, and the spec that
//! built stands.

use super::*;
use crate::pipeline::snapshot::CheckoutSnapshot;
use crate::pipeline::spec_edits::restore_spec;
use crate::spec_tidy::comment_compare::{comment_text_differs, same_without_comments};

/// The progress line of a discarded comment-only edit made by the review.
pub(super) const REVIEW_COMMENT_ONLY: &str =
    "the review changed only comments; the spec that built stands";

/// The progress line of a discarded comment-only edit made by the rpmlint fix.
pub(super) const RPMLINT_COMMENT_ONLY: &str =
    "the rpmlint fix changed only comments; the spec that built stands";

/// Whether the edit a turn just made to the spec is comment-only. `before` is
/// the spec that built; `snapshot` is the checkout as the turn found it.
///
/// The spec must differ from `before` in a comment line itself: an edit that
/// only moves blank lines or the final newline is not comment-only, and stays
/// as the turn made it. Only the spec may differ. The `.changes` files are left
/// out: an edit writes its changes entry there, and a discard puts them back
/// with the rest of the checkout. A patch, a `_service`, a source file or a new
/// file is a change of its own, so the edit is kept.
pub(super) fn is_comment_only_edit(
    snapshot: &CheckoutSnapshot,
    spec_path: &Path,
    before: &str,
) -> bool {
    if before.trim().is_empty() {
        return false;
    }
    let Ok(after) = std::fs::read_to_string(spec_path) else {
        return false;
    };
    if after == before
        || !same_without_comments(before, &after)
        || !comment_text_differs(before, &after)
    {
        return false;
    }
    let spec_name = spec_path.file_name();
    !snapshot.differs_except(|name| {
        Some(name) == spec_name || name.to_str().is_some_and(|n| n.ends_with(".changes"))
    })
}

/// Discard a comment-only edit: the checkout goes back to the snapshot and the
/// spec to `before`, the spec that built. The diff of the edit is already kept
/// as the step's `spec.diff` (written when the edit was accepted), so the
/// journal gets a note that it was discarded, and `message` is the one progress
/// line that says so.
pub(super) fn discard_comment_only_edit(
    snapshot: &CheckoutSnapshot,
    spec_path: &Path,
    before: &str,
    step: Option<&StepDir>,
    notes_path: PathBuf,
    message: &str,
    reporter: &dyn Reporter,
) {
    snapshot.restore(reporter, step);
    restore_spec(spec_path, before, step, reporter);
    append_notes(
        notes_path,
        "\n## Comment-only edit discarded\nThe edit changed only comments of the spec that \
         built, so it was not built again: the spec that built stands. Its diff is kept as \
         spec.diff in the step directory.\n",
    );
    reporter.report(&Event::Progress {
        step: step.map(|s| s.id),
        message: message.to_string(),
    });
}
