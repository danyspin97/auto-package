//! The `%dir` fix of a build that failed on unowned directories, applied to
//! the checkout (the pure edit is [`super::unowned_fix`]).

use super::unowned_fix::own_unowned_dirs;
use super::*;
use crate::pipeline::rejection::accept_or_restore_with_snapshot;
use crate::pipeline::snapshot::*;

/// Adds the `%dir` lines for a `FilesNotOwned` failure and says so. True when
/// the spec was changed and kept, so the caller rebuilds without an agent turn;
/// false when the failure is another one, nothing is left to add (the agent
/// is asked) or the gate refused the edit (the spec is put back).
pub(super) fn fix_unowned_dirs(
    ctx: &RunCtx,
    build_step: &StepDir,
    failure: &ap_core::event::BuildFailure,
) -> bool {
    let ap_core::event::BuildFailure::FilesNotOwned { paths } = failure else {
        return false;
    };
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let before = std::fs::read_to_string(spec_path).unwrap_or_default();
    let Some(fix) = own_unowned_dirs(&before, paths) else {
        return false;
    };
    let snapshot = CheckoutSnapshot::take(checkout_dir);
    if ap_core::fsx::write_atomic(spec_path, &fix.spec).is_err() {
        return false;
    }
    let accepted = accept_or_restore_with_snapshot(
        spec_path,
        &before,
        Some(&snapshot),
        build_step,
        *reporter,
        &format!(
            "\n## FixAfterBuild - %dir lines added without a turn\n{}\n",
            fix.added.join("\n")
        ),
        ws.notes(),
        &[],
        &config.rpm_name,
    );
    if !accepted {
        return false;
    }
    reporter.report(&Event::Progress {
        step: Some(build_step.id),
        message: format!(
            "{rpm_name}: the build check found directories no package owns; added %dir for {} \
             above the files in them, without a fix turn",
            fix.added.join(", ")
        ),
    });
    true
}
