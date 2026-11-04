//! The decisions of `fix` that need no OBS: which projects it refuses and the
//! exit code of a run.

use ap_build::bugfix::FixEnd;

/// Exit status of a fix that needs no change, or found none.
pub(super) const EXIT_NO_FIX: i32 = 3;
/// Exit status of a fix that needs a maintainer.
pub(super) const EXIT_NEEDS_MAINTAINER: i32 = 4;

/// Whether `project` is a maintenance or update project, where a fix is a
/// maintenance backport and not a package change.
pub(super) fn is_maintenance_project(project: &str) -> bool {
    project.ends_with(":Update")
        || project.starts_with("SUSE:SLE-")
        || project.starts_with("openSUSE:Leap:")
        || project.starts_with("openSUSE:Backports:")
}

/// The message for a maintenance project, with the command that does it.
pub(super) fn maintenance_refusal(project: &str, pkg: &str) -> String {
    format!(
        "{project} is a maintenance project: a fix there is a maintenance backport, which keeps \
         each code stream's version. Use:\n  auto-package update --project {project} --only {pkg} \
         --maintenance <CODESTREAM> --backport <URL> --bug <REF>"
    )
}

/// How a fix run ends.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// The fix built (and was submitted or not, as asked).
    Fixed,
    /// Exit 3, 4: the reason.
    Exit(i32, String),
    /// Any other failure: exit 1 with the run's own error.
    Failed,
}

/// The ending of a run from whether it succeeded and the record of the fix
/// stage. A record of another run is none (`read_fix_end` ignores it), and
/// without one the run failed before the stage or after it for another reason.
pub(super) fn ending(run_ok: bool, end: Option<&FixEnd>) -> Ending {
    let Some(end) = end else {
        return Ending::Failed;
    };
    match end.verdict.as_str() {
        "fixed" if run_ok => Ending::Fixed,
        "already_fixed" | "not_affected" | "no_fix" => {
            Ending::Exit(EXIT_NO_FIX, end.reason.clone())
        }
        "needs_maintainer" | "does_not_apply" => {
            Ending::Exit(EXIT_NEEDS_MAINTAINER, end.reason.clone())
        }
        _ => Ending::Failed,
    }
}
