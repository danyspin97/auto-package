//! The zig vendor step of an update: the vendor archive a spec reads is rebuilt
//! from the `build.zig.zon` of its new source, before the first build. A
//! refusal ends the package with its reason, and no agent turn follows.

use super::*;
use crate::pipeline::zig_vendor::{Refusal, applies, refresh_from_network};
use ap_core::event::ZigDep;

/// Step: ZigVendor, on an update of a package whose spec reads a zig vendor
/// archive (see `zig_vendor::applies`). Any other package gets no step.
pub(super) fn zig_vendor_stage(ctx: &RunCtx, st: &mut RunState) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;
    if !config.update {
        return Ok(());
    }
    // A spec that cannot be read is reported by the stages that read it.
    let Ok(spec) = std::fs::read_to_string(spec_path) else {
        return Ok(());
    };
    let refusal = match applies(&spec) {
        Ok(false) => return Ok(()),
        Ok(true) => None,
        Err(why) => Some(why),
    };
    let step_attempts = &mut st.step_attempts;
    let step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::ZigVendor,
        None,
        step_attempts,
        rpm_name.clone()
    );
    if let Some(why) = refusal {
        return Err(refused(ctx, step, why));
    }
    // The dependency archives unpack into the workspace, and go with it.
    let work = match tempfile::tempdir_in(ws.dir()) {
        Ok(work) => work,
        Err(e) => {
            return Err(refused(
                ctx,
                step,
                format!("no directory to unpack into: {e}"),
            ));
        }
    };
    let done = match refresh_from_network(checkout_dir, &spec, rpm_name, reporter, work.path()) {
        Ok(done) => done,
        Err(why) => return Err(refused(ctx, step, why)),
    };
    let Some(done) = done else {
        step.finish(StepOutcome::Ok);
        return Ok(());
    };
    if done.changed {
        let message = format!("Refresh {} for {} {}", done.file, rpm_name, config.version);
        let added = osc::add_release_changes_entry(
            checkout_dir,
            &config.version,
            &message,
            Emit::new(reporter, step.id),
        );
        if let Err(e) = added {
            step.finish(StepOutcome::Failed);
            return Err(ctx.failed_osc(&e));
        }
    }
    reporter.report(&Event::ZigVendor {
        step: step.id,
        pkg: rpm_name.clone(),
        file: done.file,
        changed: done.changed,
        deps: done
            .deps
            .into_iter()
            .map(|d| ZigDep {
                name: d.name,
                url: d.url,
                hash: d.hash,
            })
            .collect(),
    });
    step.finish(StepOutcome::Ok);
    Ok(())
}

/// The package ends with `why`, as its failure kind: the refresh is refused,
/// and the step failed. A plain reason is an `Other` failure.
fn refused(ctx: &RunCtx, step: StepGuard<'_>, why: impl Into<Refusal>) -> PipelineResult {
    let why = why.into();
    step.finish(StepOutcome::Failed);
    ctx.failed_as(
        why.kind,
        format!("the zig vendor archive cannot be refreshed: {}", why.msg),
    )
}
