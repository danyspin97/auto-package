//! The loop guard on sources: an edit may not declare a `Source` again when the
//! spec already declares it, or when an earlier accepted attempt of this run
//! declared it (an accepted attempt stays in the spec, so the spec before a turn
//! carries every source the earlier attempts added).

use super::*;
use crate::pipeline::render::*;

/// The value of a `SourceN:` declaration (the URL or file name it names), or
/// `None` for any other line. A comment never counts.
pub(super) fn source_value(line: &str) -> Option<&str> {
    let (key, value) = line.trim_start().split_once(':')?;
    let digits = key.trim_end().to_ascii_lowercase();
    let digits = digits.strip_prefix("source")?;
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value.split_whitespace().next()
}

/// The sources an edit declares again: each value that the edited spec
/// declares more than once and that it declares more often than the spec before
/// the edit did. A source the spec already had twice, left alone, is not one.
pub(super) fn repeated_sources(before: &str, after: &str) -> Vec<String> {
    let count = |text: &str, value: &str| {
        text.lines()
            .filter_map(source_value)
            .filter(|v| *v == value)
            .count()
    };
    let mut repeated: Vec<String> = Vec::new();
    for value in after.lines().filter_map(source_value) {
        let now = count(after, value);
        if now > 1 && now > count(before, value) && !repeated.iter().any(|r| r == value) {
            repeated.push(value.to_string());
        }
    }
    repeated
}

/// The acceptance check for the sources of an edit: a spec that declares a
/// source again is rolled back the way a rejected spec is, and the rejection is
/// noted. Returns the reason when the edit was rolled back.
pub(super) fn reject_repeated_sources(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
) -> Option<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let repeated = repeated_sources(before, &after);
    if repeated.is_empty() {
        return None;
    }
    let problem = format!(
        "the edit declares a source again: {}. A source is declared once; the spec already \
         has it from an earlier attempt or from its own text. A dependency that the build \
         cannot fetch is not fixed by declaring it twice.",
        repeated.join(", ")
    );
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::RepeatedSource {
            urls: repeated.clone(),
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
            "\n## Edit rejected — a source is declared again\n{problem}\nThe edit was rolled \
             back. Do not add the same source a second time.\n"
        ),
    );
    Some(problem)
}
