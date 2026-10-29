use super::*;

/// Drops the fix-loop and risk flags `ws` recorded for another version than
/// `version`, and records `version` as the one they are for (see
/// [`run_package`]).
pub(super) fn start_flags_for_version(ws: &Workspace, version: &str, reporter: &dyn Reporter) {
    update_state(ws, reporter, |state| {
        let other = state.flags_version.as_deref().is_some_and(|v| v != version);
        if other {
            state.fix_flags.clear();
            state.fix_flags_run_id = None;
            state.risk_flags.clear();
            state.risk_flags_run_id = None;
        }
        state.flags_version = Some(version.to_string());
    });
}

/// Records the risk flags of the change about to be committed: what OBS holds
/// (`baseline`) against the files on disk. Read from the files, not from what
/// the agent said.
pub(super) fn settle_risk_flags(ctx: &RunCtx, baseline: &Path) {
    use crate::risk_flags::{Files, archive_files, from_change, patch_names};
    let spec_name = ctx.spec_path.file_name().unwrap_or_default();
    let read_patches = |spec: &str, read: &dyn Fn(&str) -> Option<String>| {
        patch_names(spec)
            .into_iter()
            .filter_map(|name| read(&name).map(|text| (name, text)))
            .collect()
    };
    let base_spec = baseline_text(baseline, spec_name);
    let before = Files {
        spec: base_spec.clone(),
        service: baseline_text(baseline, "_service"),
        patches: read_patches(base_spec.as_deref().unwrap_or(""), &|n| {
            baseline_text(baseline, n)
        }),
    };
    let fin_spec = std::fs::read_to_string(&ctx.spec_path).ok();
    let checkout = &ctx.checkout_dir;
    let after = Files {
        spec: fin_spec.clone(),
        service: std::fs::read_to_string(checkout.join("_service")).ok(),
        patches: read_patches(fin_spec.as_deref().unwrap_or(""), &|n| {
            std::fs::read_to_string(checkout.join(n)).ok()
        }),
    };
    let tarball = find_tarball(checkout, &ctx.config.version);
    let archive = archive_files(tarball.as_deref());
    let found = from_change(&before, &after, &archive);
    update_state(&ctx.ws, ctx.reporter, |st| {
        crate::risk_flags::settle(st, &ctx.config.run_id, found)
    });
}

/// Records the fix-loop flags of the spec about to be committed: the change
/// from the OBS baseline (`baseline` holds the files OBS has) to the final spec.
/// The `blocked` answers the run recorded stay.
pub(super) fn settle_fix_flags(ctx: &RunCtx, baseline: &Path, step: ap_core::event::StepId) {
    let spec_name = ctx.spec_path.file_name().unwrap_or_default();
    let before = baseline_text(baseline, spec_name);
    let after = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let found = crate::fix_flags::from_spec_diff(before.as_deref(), &after);
    update_state(&ctx.ws, ctx.reporter, |st| {
        crate::fix_flags::settle(st, &ctx.config.run_id, found)
    });
    // The flags go into the changelog entry, one line each under its bullet.
    let lines: Vec<String> = PkgState::load(&ctx.ws)
        .fix_flags
        .iter()
        .map(crate::fix_flags::FixFlag::line)
        .collect();
    warn_if_err(
        ctx.reporter,
        "adding the fix-loop flags to the .changes entry",
        osc::set_flag_lines(
            &ctx.checkout_dir,
            &ctx.config.version,
            &lines,
            Emit::new(ctx.reporter, step),
        ),
    );
}
