//! Build flags of a branch package, copied from its origin after the branch is
//! made or found in the branch project.

use ap_core::event::Event;
use ap_core::sink::Reporter;

/// Copy the build flags of `origin/pkg` onto `branch/pkg`, so the branch skips
/// the repositories its origin skips. A failure only warns: the branch then
/// builds with the flags it has.
pub(crate) fn copy_build_flags(
    reporter: &dyn Reporter,
    branch: &str,
    origin: &str,
    pkg: &str,
    emit: ap_build::proc::Emit<'_>,
) {
    let say = |message: String| {
        reporter.report(&Event::Progress {
            step: None,
            message,
        })
    };
    let warn = |message: String| {
        reporter.report(&Event::Warning {
            step: None,
            message,
        })
    };
    match ap_build::osc::sync_build_flags(origin, branch, pkg, emit) {
        Ok(ap_build::osc::FlagSync::NoFlags) => {}
        Ok(ap_build::osc::FlagSync::Equal) => say(format!(
            "{branch}/{pkg} already has the build flags of {origin}"
        )),
        Ok(ap_build::osc::FlagSync::Copied(kinds)) => say(format!(
            "copied the build flags of {origin}/{pkg} ({}) to {branch}/{pkg}",
            kinds.join(", ")
        )),
        Err(e) => warn(format!(
            "could not copy the build flags of {origin}/{pkg} to {branch}/{pkg}, \
             building it with the flags it has: {e}"
        )),
    }
}
