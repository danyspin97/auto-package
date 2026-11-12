//! Classifying a failure from its log.

use super::*;

/// `BuildFailure`'s variants (`PrepFailed`/`BuildFailed`/`InstallFailed`/
/// `CheckFailed`) name *phases*, not failure *shapes* — so this has to switch
/// on `failure.phase`, not `failure.genre`. The previous version switched on
/// genre (`ShellError` always became `PrepFailed`, `PythonTraceback` always
/// became `BuildFailed`, regardless of which section actually failed) and
/// genres it didn't recognize a mapping for — `Genre::Unknown`, which is
/// exactly what a plain GCC/Clang compiler diagnostic or a linker error looks
/// like, since neither is a Python traceback or a `script: line N:` shell
/// error — fell all the way to `Unclassified` even when the phase was known
/// perfectly well (a real `%build` compiler failure in `triton`,
/// `python-hf-xet` and `python-torchcodec` all did this). Phase is already
/// the more reliable signal here: `rpm_reported_failure` gets it from rpm's
/// own `(%section)` in "Bad exit status from ... (%section)", so use it
/// directly and only fall back to genre for phases that aren't one of the
/// four rpm sections at all (rpm's own post-install file-list check often
/// gets attributed to whichever section ran last, e.g. `%license`).
pub(super) fn classify_failure(
    extracted: &crate::log::ExtractedLog,
) -> ap_core::event::BuildFailure {
    use crate::log::{Genre, Phase, unowned::unowned_dirs};
    let Some(failure) = &extracted.failure else {
        return ap_core::event::BuildFailure::Unclassified {
            raw: unclassified_raw(extracted),
        };
    };
    // The excerpt keeps every `- /path` bullet under rpm's `directories not
    // owned by a package:` header. Only a post-build check can carry one, and
    // it wins over the phase: the check runs after the last section, so its
    // phase is whichever section ran last.
    let unowned = if failure.genre == Genre::PostBuildCheck {
        unowned_dirs(&failure.excerpt)
    } else {
        Vec::new()
    };
    match &failure.phase {
        _ if !unowned.is_empty() => ap_core::event::BuildFailure::FilesNotOwned { paths: unowned },
        Phase::Section(s) if s == "%prep" => ap_core::event::BuildFailure::PrepFailed,
        Phase::Section(s) if s == "%build" => ap_core::event::BuildFailure::BuildFailed,
        Phase::Section(s) if s == "%install" => ap_core::event::BuildFailure::InstallFailed,
        // rpm's file-list check runs after the last section and is attributed
        // to it, so a `%check` that ended the rpm run can carry a file complaint.
        // Only a section's own `Bad exit status` is a check failure.
        Phase::Section(s) if s == "%check" && !failure.is_post_build_check() => {
            ap_core::event::BuildFailure::CheckFailed
        }
        // A source service failed before any rpm section ran: the sources were
        // never made. Not an install failure, whatever else the log says.
        _ if failure.genre == Genre::SourceService => ap_core::event::BuildFailure::Unclassified {
            raw: unclassified_raw(extracted),
        },
        _ if failure.genre == Genre::PostBuildCheck || failure.phase == Phase::PostBuild => {
            ap_core::event::BuildFailure::InstallFailed
        }
        // Any other named rpm section — `%generate_buildrequires` (a real
        // case for pyproject-rpm-macros' dynamic BuildRequires resolution),
        // `%conf`, `%mkbuilddir`, etc. These all run before `%build` as part
        // of setting up the build, so `PrepFailed` is the closest existing
        // bucket rather than falling all the way through to `Unclassified`
        // and losing the phase signal `failure.phase` already captured.
        Phase::Section(_) => ap_core::event::BuildFailure::PrepFailed,
        // Phase::Setup: the failure landed before any spec section began —
        // buildroot resolution, workflow aborted mid-setup, or OBS never
        // produced a real log at all. Not a code-shaped failure, but still
        // worth reporting more than an empty string (which is what we used
        // to do when `error_lines` was empty — common for a build that
        // never progressed far enough to emit an rpm `error:` line, which
        // in turn looked like an extraction bug to users and buried the
        // real problem).
        _ => ap_core::event::BuildFailure::Unclassified {
            raw: unclassified_raw(extracted),
        },
    }
}

#[cfg(test)]
mod classify_tests;
