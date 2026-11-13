//! The acceptance check for a `%python_expand ` line that an edit puts inside a
//! braced `%{python_expand ...}` block (convention 19 (section 80)). The edit is rolled back
//! the way a rejected spec is; a line the spec already had before the turn is
//! inherited and left alone.

use super::*;

/// Returns the reason when the edit was rolled back for such a line.
pub(super) fn reject_nested_python_expand(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
) -> Option<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let fresh = crate::spec::fresh_nested_python_expand_lines(before, &after);
    if fresh.is_empty() {
        return None;
    }
    let lines: Vec<String> = fresh
        .iter()
        .map(|(n, text)| format!("line {n}: {}", text.trim()))
        .collect();
    let problem = format!(
        "the edit puts a `%python_expand` line inside a braced `%{{python_expand ...}}` block: {}. \
         Inside a braced block each line is one plain shell command, so drop the \
         `%python_expand ` prefix from each of those lines and keep the block's opener as the \
         origin has it.",
        lines.join("; ")
    );
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::NestedPythonExpand { lines },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — a %python_expand line inside a braced block\n{problem}\n\
             The edit was rolled back. Do not nest a %python_expand line in a braced block.\n"
        ),
    );
    Some(problem)
}

/// Returns the reason when the edit reshaped a braced block the origin has
/// (convention 19 (section 119)): the block was removed, merged, split or
/// turned into `%python_expand` lines. No evidence clears this check; a line
/// inside the block is the edit's to change.
pub(super) fn reject_reshaped_python_expand_block(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
) -> Option<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let first_lines = crate::spec::reshaped_python_expand_blocks(before, &after);
    if first_lines.is_empty() {
        return None;
    }
    let names: Vec<String> = first_lines.iter().map(|l| format!("`{l}`")).collect();
    let problem = format!(
        "the edit reshapes the origin's braced `%{{python_expand ...}}` block that opens {}: \
         a block was removed, merged, split, or turned into `%python_expand` lines. This is an \
         unnecessary rewrite. Keep the origin's block and change only the lines that need it; \
         a `%fdupes` call stays inside the block, since the block already runs per flavor. \
         No evidence clears this check.",
        names.join(", ")
    );
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::ReshapedPythonExpandBlock { first_lines },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — an origin's braced %python_expand block was reshaped\n{problem}\n\
             The edit was rolled back. Keep the block; edit only the line that needs it.\n"
        ),
    );
    Some(problem)
}
