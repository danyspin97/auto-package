//! The gate on an update's `Source0` (convention 32): a recipe that builds from
//! its release's sdist keeps that sdist on an update. A switch of `Source0` to
//! the release's wheel is refused, whether the generator made it or an agent
//! edit did: the wheel is for a release with no buildable source, and the switch
//! that is allowed for a build that cannot run here is `use_upstream_wheel`
//! (convention 28 Exception 4), with the build log line that shows it.

use super::*;
use crate::pipeline::render::*;

/// The value of `Source0:` (or `Source:`) in `spec`: the first token after it.
pub(super) fn main_source(spec: &str) -> Option<&str> {
    spec.lines().find_map(|line| {
        let (key, value) = line.trim_start().split_once(':')?;
        let key = key.trim_end().to_ascii_lowercase();
        if key == "source" || key == "source0" {
            value.split_whitespace().next()
        } else {
            None
        }
    })
}

/// Whether `value` names an sdist archive (a source tarball or zip).
pub(super) fn is_sdist_name(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    [
        ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".tar.zst", ".tar", ".zip",
    ]
    .iter()
    .any(|ext| v.ends_with(ext))
}

/// Whether `value` names a wheel.
pub(super) fn is_wheel_name(value: &str) -> bool {
    value.to_ascii_lowercase().ends_with(".whl")
}

/// Whether the spec `after` switches `Source0` from an sdist (in `before`) to a
/// wheel. A new package (no `Source0` before) is not a switch.
pub(super) fn sdist_to_wheel(before: &str, after: &str) -> bool {
    matches!(
        (main_source(before), main_source(after)),
        (Some(b), Some(a)) if is_sdist_name(b) && is_wheel_name(a)
    )
}

/// The reason a switch from `before` to `after` is refused, when it is one (see
/// [`sdist_to_wheel`]); `None` when `applies` is false (a new package, or a
/// package asked for its wheel).
pub(super) fn switch_refusal(applies: bool, before: &str, after: &str) -> Option<String> {
    if !applies || !sdist_to_wheel(before, after) {
        return None;
    }
    let from = main_source(before).unwrap_or_default();
    let to = main_source(after).unwrap_or_default();
    Some(format!(
        "Source0 moves from the release's sdist ({from}) to its wheel ({to}). This recipe \
         builds from the sdist, and convention 32 keeps an update on it: a wheel is for a \
         release with no buildable source. A wheel switch needs use_upstream_wheel with a \
         build.log line that proves a compiler or SDK is missing here (convention 28 \
         Exception 4)."
    ))
}

/// The acceptance check of an agent edit on an update: a `Source0` switch from
/// the sdist to the wheel is rolled back the way a rejected spec is. Returns the
/// reason when the edit was rolled back.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_sdist_to_wheel(
    applies: bool,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
) -> Option<String> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_else(|_| before.to_string());
    let problem = switch_refusal(applies, before, &after)?;
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::SdistToWheel {
            from: main_source(before).unwrap_or_default().to_string(),
            to: main_source(&after).unwrap_or_default().to_string(),
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
            "\n## Edit rejected — an update switches its sdist to a wheel\n{problem}\nThe edit \
             was rolled back.\n"
        ),
    );
    Some(problem)
}

#[cfg(test)]
#[path = "sdist_wheel_tests.rs"]
mod tests;
