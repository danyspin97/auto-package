//! The acceptance check for a fixup an edit adds to a build phase without a comment
//! saying why (convention 40). The check itself is `crate::spec::unexplained_fixups`;
//! the edit is rolled back the way a rejected spec is, and a fixup the origin spec
//! already has is inherited and left alone.

use super::*;

/// Returns the reason when the edit was rolled back for such a fixup.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_unexplained_fixup(
    spec_path: &Path,
    before: &str,
    after: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    package: &str,
) -> Option<String> {
    let found = crate::spec::unexplained_fixups(before, after, package);
    if found.is_empty() {
        return None;
    }
    let problem = format!(
        "the edit adds a fixup with no comment saying why: {}. A line the edit adds to %prep, \
         %build, %install or %check that edits a file or the environment (`sed -i`, `perl -pi`, \
         `chmod`, `mv`, `ln -s`, `echo >`, `cp`, `rm` of a path under the build root or the \
         source tree, `export`) needs a one-line `#` comment directly above it, or above the \
         `%{{python_expand` block it sits in, that says why: the failing step, the file, the \
         reason, in words and with no `%` macro.",
        found.join("; ")
    );
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::UnexplainedFixup {
            lines: found.clone(),
        },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — a fixup has no comment saying why\n{problem}\n\
             The edit was rolled back. Add the comment above each such line and redo the edit.\n"
        ),
    );
    Some(problem)
}
