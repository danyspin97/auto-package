//! The settling of refresh-patch checks: the quilt check that removes the patches
//! quilt proves applied, and the keeping of the edits a failed refresh turn made
//! that quilt proves apply.

use super::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// One quilt check of the patches, with the removals it proves settled: a patch
/// quilt proves already applied is removed now (certainly right, and nothing
/// for a turn to do). The rest of the report is what is left.
pub(super) fn check_and_settle(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    lines: &mut Vec<String>,
    check: &mut PatchCheckFn<'_>,
    refresh: crate::patch_check::Refresh,
) -> crate::patch_check::CheckReport {
    let mut report = check(ctx, refresh);
    let proven: Vec<String> = report.already_applied().map(|p| p.name.clone()).collect();
    lines.extend(remove_proven_patches(ctx, step, &proven));
    report.patches.retain(|p| {
        !matches!(
            p.status,
            crate::patch_check::PatchStatus::AlreadyApplied { .. }
        )
    });
    report
}

/// After a refresh turn that failed: keeps each patch the turn changed that
/// quilt proves applies, checked one at a time with the others back at their
/// state before the turn, and rolls back the rest. Returns the names kept; a
/// note for each patch goes to `lines`.
pub(super) fn keep_proven_edits(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    check: &mut PatchCheckFn<'_>,
    lines: &mut Vec<String>,
    snapshot: &CheckoutSnapshot,
    before_spec: &str,
    before_files: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
) -> Vec<String> {
    use crate::patch_check::{PatchStatus, Refresh};
    // The turn's spec is still on disk here: its top directory names the headers.
    let top = header_top(ctx);
    let edited: Vec<(String, Vec<u8>)> = patch_files(before_spec, &ctx.checkout_dir)
        .into_iter()
        .filter_map(|(name, now)| {
            let before = before_files.get(&name).cloned().flatten();
            match now {
                Some(bytes) if before.as_deref() != Some(bytes.as_slice()) => {
                    Some((name, with_top_dir(bytes, top.as_deref())))
                }
                _ => None,
            }
        })
        .collect();
    let mut kept: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, bytes) in &edited {
        restore_turn_edits(ctx, snapshot, before_spec);
        write_or_warn(ctx.checkout_dir.join(name), bytes);
        let report = check_and_settle(ctx, step, &mut Vec::new(), check, Refresh::No);
        match report
            .patches
            .iter()
            .find(|p| &p.name == name)
            .map(|p| &p.status)
        {
            Some(PatchStatus::Applies) => kept.push((name.clone(), bytes.clone())),
            Some(status) => lines.push(format!(
                "- {name}: rolled back, its edit does not apply: {}",
                status_reason(status)
            )),
            None => lines.push(format!("- {name}: rolled back, not in the check")),
        }
    }
    restore_turn_edits(ctx, snapshot, before_spec);
    for (name, bytes) in &kept {
        write_or_warn(ctx.checkout_dir.join(name), bytes);
        lines.push(format!("- {name}: kept; quilt proves its edit applies"));
    }
    kept.into_iter().map(|(name, _)| name).collect()
}

/// The archive's top directory, as the spec on disk names it (`%autosetup -n`).
fn header_top(ctx: &RunCtx) -> Option<String> {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    crate::spec::autosetup_expected_dir(&spec, &ctx.config.rpm_name, &ctx.config.version)
}

/// `bytes` with quilt's `source/` header prefixes made `top` (see
/// `rewrite_header_prefixes`); unchanged when there is no top or nothing to do.
fn with_top_dir(bytes: Vec<u8>, top: Option<&str>) -> Vec<u8> {
    top.and_then(|top| crate::patch_check::rewrite_header_prefixes(&bytes, top))
        .unwrap_or(bytes)
}

/// After a refresh turn: each patch the turn changed or added has the archive's
/// top directory in its headers in place of quilt's `source/` and
/// `source.orig/`, written back only when that changes it. The patches are the
/// ones the spec on disk declares, so a patch the turn added is included.
pub(super) fn normalise_refreshed_headers(
    ctx: &RunCtx,
    before_files: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
) {
    let Some(top) = header_top(ctx) else {
        return;
    };
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    for (name, now) in patch_files(&spec, &ctx.checkout_dir) {
        let before = before_files.get(&name).cloned().flatten();
        if before == now {
            continue;
        }
        if let Some(bytes) = now.and_then(|b| crate::patch_check::rewrite_header_prefixes(&b, &top))
        {
            write_or_warn(ctx.checkout_dir.join(&name), &bytes);
        }
    }
}

/// The checkout and the spec as they were before the turn.
fn restore_turn_edits(ctx: &RunCtx, snapshot: &CheckoutSnapshot, before_spec: &str) {
    snapshot.restore(ctx.reporter, None);
    restore_spec(&ctx.spec_path, before_spec, None, ctx.reporter);
}

/// What quilt said about a patch that is not kept, in one line.
pub(super) fn status_reason(status: &crate::patch_check::PatchStatus) -> String {
    use crate::patch_check::PatchStatus;
    match status {
        PatchStatus::Applies => "it applies".into(),
        PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. } => {
            "it applies only with fuzz".into()
        }
        PatchStatus::AppliesWithOffsetOrFuzz { fuzz: false, .. } => {
            "it applies only with an offset".into()
        }
        PatchStatus::Fails { output, .. } => output
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("Applying"))
            .unwrap_or("quilt does not apply it")
            .to_string(),
        PatchStatus::AlreadyApplied { .. } => {
            "quilt says it is already applied; a failed turn removes nothing".into()
        }
        PatchStatus::NotChecked { reason } => reason.clone(),
    }
}
