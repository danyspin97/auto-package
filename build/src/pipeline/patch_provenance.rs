//! The upstream provenance of the carried patches (plan 4.5, 4.6). A run that
//! has the switch set and is a lint (not an update) checks each declared patch:
//! a patch whose upstream request is merged in the packaged release is dropped,
//! only with its removal proof; the header and the spec comment of each
//! upstream patch are written; every verdict is a note in the package notes.
//! An update replaces a moved open request's patch in its patch loop instead
//! (see [`try_upstream_revisions`]). A provenance reason is never a failure.

use super::*;
use crate::patch_provenance::{self as pp, ForgeNet, LiveForge, Provenance, Verdict};
use crate::pipeline::patch_review::{
    PatchReviewEnd, ReviewTurn, review_patch_rounds, stage_applies,
};
use crate::pipeline::patch_rules::reject_patch_rules;
use crate::pipeline::rejection::reject_patch_escape;
use crate::pipeline::removal_proof::drop_proof;
use crate::pipeline::review_marks::mark_reviewer_rewrite;
use crate::pipeline::snapshot::CheckoutSnapshot;

/// The lint stage: the forge is asked through [`LiveForge`]. Does nothing unless
/// the run set `patch_provenance`.
pub(super) fn provenance_stage(ctx: &RunCtx) {
    if !ctx.config.patch_provenance {
        return;
    }
    let hosts = crate::allowed_hosts::load_default();
    let forge = LiveForge {
        package: &ctx.config.rpm_name,
        hosts: &hosts,
    };
    provenance_stage_with(ctx, &forge);
}

/// [`provenance_stage`] with the forge given (tests use a fake).
pub(super) fn provenance_stage_with(ctx: &RunCtx, net: &dyn ForgeNet) {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let files = patch_files(&spec, &ctx.checkout_dir);
    let patches: Vec<(String, String)> = files
        .iter()
        .filter_map(|(name, content)| {
            let text = String::from_utf8(content.clone()?).ok()?;
            Some((name.clone(), text))
        })
        .collect();
    if patches.is_empty() {
        return;
    }
    let changes = std::fs::read_to_string(
        ctx.checkout_dir
            .join(format!("{}.changes", ctx.config.rpm_name)),
    )
    .unwrap_or_default();
    let inputs = pp::Inputs {
        pkg: &ctx.config.rpm_name,
        spec: &spec,
        changes: &changes,
        version: &ctx.config.version,
        repo: crate::bugfix::upstream_repo_of_spec(&spec),
        patches: &patches,
    };
    let provenance = pp::assess(&inputs, net, crate::upstream_cache::refreshing());
    let mut spec_now = spec;
    let mut notes: Vec<String> = Vec::new();
    for p in &provenance {
        if let Some(true) = act_on(ctx, &mut spec_now, p, &mut notes) {
            continue;
        }
        let text = patches
            .iter()
            .find(|(n, _)| *n == p.name)
            .map(|(_, t)| t.as_str())
            .unwrap_or_default();
        write_header(ctx, &mut spec_now, p, text, &mut notes);
    }
    if !notes.is_empty() {
        append_notes(
            ctx.ws.notes(),
            &format!("\n## Patch provenance\n{}\n", notes.join("\n")),
        );
    }
}

/// Whether the update's patch loop asks for upstream revisions: an update that
/// has the switch, has an agent, and has failing patches (plan 4.6).
pub(super) fn upstream_revisions_wanted(config: &PipelineConfig, failing: &[String]) -> bool {
    config.patch_provenance && config.update && !config.no_agent && !failing.is_empty()
}

/// Whether a moved request's revision was taken for its patch.
#[derive(Debug, PartialEq)]
enum Replaced {
    Taken,
    Kept,
}

/// Takes the current revision of a moved open request for the carried patch
/// `p` (plan 4.3). The revision keeps the carried header lines the maintainer
/// wrote; it must pass the escape and patch-rule gates of an edit, apply to the
/// new sources, and be accepted by a PatchReview turn. Otherwise the carried
/// bytes come back with a note: the patch is not a failure.
#[allow(clippy::too_many_arguments)]
fn replace_with_revision(
    ctx: &RunCtx,
    st: &mut RunState,
    step_id: ap_core::event::StepId,
    check: &mut PatchCheckFn<'_>,
    review: &mut ReviewTurn<'_>,
    net: &dyn ForgeNet,
    p: &Provenance,
    text: &str,
    notes: &mut Vec<String>,
) -> Result<Replaced, PipelineResult> {
    let (Verdict::HeadMoved { head, patch_url }, Some(origin)) = (&p.verdict, &p.origin) else {
        return Ok(Replaced::Kept);
    };
    let name = p.name.as_str();
    let short = &head[..head.len().min(12)];
    let fresh = match net.patch_text(patch_url) {
        Ok(t) => t,
        Err(why) => {
            notes.push(format!(
                "- {name}: the request's revision cannot be read: {why}"
            ));
            return Ok(Replaced::Kept);
        }
    };
    let carried = crate::patch_provenance::carried_into(text, &fresh);
    let new_text = pp::header_for(p, &carried).unwrap_or(carried);
    if let Err(why) = begin_turn(&ctx.ws, &ctx.spec_path) {
        refused(
            ctx,
            step_id,
            name,
            &format!("the turn could not start: {why}"),
            notes,
        );
        return Ok(Replaced::Kept);
    }
    let old = text.as_bytes().to_vec();
    let spec_before = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let snapshot = CheckoutSnapshot::take(&ctx.checkout_dir);
    write_or_warn(ctx.checkout_dir.join(name), &new_text);
    let step = StepDir {
        path: ctx.ws.dir().join("patch-provenance"),
        id: step_id,
    };
    let _ = std::fs::create_dir_all(&step.path);
    let gate = reject_patch_escape(
        &ctx.spec_path,
        &spec_before,
        Some(&snapshot),
        &step,
        ctx.reporter,
        ctx.ws.notes(),
        &[],
    )
    .or_else(|| {
        reject_patch_rules(
            &ctx.spec_path,
            &spec_before,
            Some(&snapshot),
            &step,
            ctx.reporter,
            ctx.ws.notes(),
            &[],
            false,
        )
    });
    let mut lines: Vec<String> = Vec::new();
    let end = match gate {
        Some(why) => Ok(PatchReviewEnd::Rejected(format!(
            "the gates refuse it: {why}"
        ))),
        None => {
            let cause = format!("replaced by the current revision {short} of {}", origin.url);
            let mut applies = |c: &RunCtx, n: &str| stage_applies(c, &mut *check, n);
            match applies(ctx, name) {
                Err(why) => Ok(PatchReviewEnd::Rejected(format!(
                    "the revision does not apply: {why}"
                ))),
                Ok(()) => review_patch_rounds(
                    ctx,
                    st,
                    step_id,
                    name,
                    Some(&old),
                    &cause,
                    &mut applies,
                    review,
                    &mut lines,
                ),
            }
        }
    };
    notes.extend(lines);
    let back = |ctx: &RunCtx| {
        write_or_warn(ctx.checkout_dir.join(name), &old);
        if !take_unsettled() {
            end_turn(&ctx.ws);
        }
    };
    match end {
        Ok(PatchReviewEnd::Accepted { rewrote }) => {
            if rewrote && let Err(why) = mark_reviewer_rewrite(&ctx.ws, &ctx.checkout_dir, name) {
                back(ctx);
                return Err(ctx.failed_as(FailureKind::NeedsMaintainer, why));
            }
            // Recorded before the turn is forgotten: a crash between the two
            // must not leave a replacement the risk flag cannot see.
            if let Err(why) = mark_replaced(&ctx.ws, &ctx.checkout_dir, name) {
                back(ctx);
                return Err(ctx.failed_as(FailureKind::NeedsMaintainer, why));
            }
            if !take_unsettled() {
                end_turn(&ctx.ws);
            }
            notes.push(format!(
                "- {name}: replaced by the current revision {short}"
            ));
            Ok(Replaced::Taken)
        }
        Ok(other) => {
            back(ctx);
            refused(ctx, step_id, name, &format!("{other:?}"), notes);
            Ok(Replaced::Kept)
        }
        Err(PipelineResult::Failed {
            kind: FailureKind::Exhausted,
            reason,
            ..
        }) => {
            back(ctx);
            refused(
                ctx,
                step_id,
                name,
                &format!("no review turn was left ({reason})"),
                notes,
            );
            Ok(Replaced::Kept)
        }
        Err(stop) => {
            back(ctx);
            Err(stop)
        }
    }
}

/// A revision that was not taken: a warning for the run and a note for the
/// package. The carried patch stays; this is not a failure of the package.
fn refused(
    ctx: &RunCtx,
    step_id: ap_core::event::StepId,
    name: &str,
    why: &str,
    notes: &mut Vec<String>,
) {
    ctx.reporter.report(&Event::Warning {
        step: Some(step_id),
        message: format!(
            "{}: the current revision of {name} was not taken, the carried patch stays: {why}",
            ctx.rpm_name
        ),
    });
    notes.push(format!(
        "- {name}: the current revision was not taken: {why}"
    ));
}

/// What the verdict asks for. `Some(true)` when the patch was dropped; `Some(false)`
/// when it stays with a note; `None` when the verdict asks nothing here.
fn act_on(
    ctx: &RunCtx,
    spec: &mut String,
    p: &Provenance,
    notes: &mut Vec<String>,
) -> Option<bool> {
    let url = p
        .origin
        .as_ref()
        .map(|o| o.url.as_str())
        .unwrap_or_default();
    let name = p.name.as_str();
    match &p.verdict {
        Verdict::Redundant { tag } => Some(drop_merged(ctx, spec, name, url, tag, notes)),
        Verdict::ReleasedLater { tag, version } => {
            notes.push(format!(
                "- {name}: merged upstream in {tag}, after the packaged release: kept (a newer version takes it: {version})"
            ));
            Some(false)
        }
        Verdict::HeadMoved { .. } => {
            notes.push(format!(
                "- {name}: {url} moved with different code: run update to take its current revision"
            ));
            Some(false)
        }
        Verdict::MovedNotComparable(why) => {
            notes.push(format!(
                "- {name}: {url} moved and cannot be compared: {why}"
            ));
            Some(false)
        }
        Verdict::Closed => {
            notes.push(format!(
                "- {name}: {url} is closed without a merge: kept (a maintainer decides)"
            ));
            Some(false)
        }
        Verdict::NotChecked(why) => {
            notes.push(format!("- {name}: upstream state not checked: {why}"));
            Some(false)
        }
        Verdict::Nothing => None,
    }
}

/// Drop a patch the packaged release has, when the removal proof holds. The
/// proof is the same one a removal needs; without it the patch stays.
fn drop_merged(
    ctx: &RunCtx,
    spec: &mut String,
    name: &str,
    url: &str,
    tag: &str,
    notes: &mut Vec<String>,
) -> bool {
    let files = patch_files(spec, &ctx.checkout_dir);
    let Some(proof) = drop_proof(&ctx.checkout_dir, &ctx.config.version, spec, &files, name) else {
        ctx.reporter.report(&Event::Warning {
            step: None,
            message: format!(
                "{}: {name} is merged upstream in {tag}, but the removal proof fails (merged in another form?): kept for a maintainer",
                ctx.rpm_name
            ),
        });
        notes.push(format!(
            "- {name}: merged upstream in {tag} ({url}); kept: the removal proof fails"
        ));
        return false;
    };
    let names = [name.to_string()];
    let without = without_patches(spec, &names);
    write_or_warn(&ctx.spec_path, &without);
    drop_series_lines(&ctx.checkout_dir, &names);
    let _ = std::fs::remove_file(ctx.checkout_dir.join(name));
    *spec = without;
    ctx.reporter.report(&Event::Warning {
        step: None,
        message: format!(
            "{}: removed {name}: merged upstream in {tag}, and the packaged release has it",
            ctx.rpm_name
        ),
    });
    warn_if_err(
        ctx.reporter,
        "adding the .changes entry for a patch",
        osc::add_release_changes_entry(
            &ctx.checkout_dir,
            &ctx.config.version,
            &crate::patch_check::proven_drop_bullet(name, proof),
            Emit::quiet(),
        ),
    );
    notes.push(format!(
        "- {name}: removed by the pipeline: merged upstream in {tag} ({url})"
    ));
    true
}

/// Write the header and the spec comment a patch is due, if any.
fn write_header(
    ctx: &RunCtx,
    spec: &mut String,
    p: &Provenance,
    text: &str,
    notes: &mut Vec<String>,
) {
    if let Some(new_text) = pp::header_for(p, text) {
        write_or_warn(ctx.checkout_dir.join(&p.name), new_text);
        notes.push(format!("- {}: upstream header written", p.name));
    }
    if let Some(new_spec) = pp::spec_comment_for(p, spec) {
        write_or_warn(&ctx.spec_path, &new_spec);
        *spec = new_spec;
    }
}

/// The update's patch loop (plan 4.6), after the quilt pre-pass and before any
/// rebase turn: each failing carried patch whose request moved takes the
/// request's revision (the same gates as [`replace_with_revision`]); a patch the
/// packaged release merged is dropped with its proof; a patch a later release
/// merged gets a hint for the rounds. A taken revision is in `accepted`, so the
/// review stage does not review it twice, and its name is in `refreshed`.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_upstream_revisions(
    ctx: &RunCtx,
    st: &mut RunState,
    step_id: ap_core::event::StepId,
    check: &mut PatchCheckFn<'_>,
    review: &mut ReviewTurn<'_>,
    net: &dyn ForgeNet,
    failing: &[String],
    accepted: &mut std::collections::BTreeMap<String, Vec<u8>>,
    refreshed: &mut std::collections::BTreeSet<String>,
    lines: &mut Vec<String>,
) -> Result<(), PipelineResult> {
    let mut spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let patches: Vec<(String, String)> = patch_files(&spec, &ctx.checkout_dir)
        .into_iter()
        .filter(|(name, _)| failing.contains(name))
        .filter_map(|(name, content)| {
            let text = String::from_utf8(content?).ok()?;
            Some((name, text))
        })
        .collect();
    if patches.is_empty() {
        return Ok(());
    }
    let changes = std::fs::read_to_string(
        ctx.checkout_dir
            .join(format!("{}.changes", ctx.config.rpm_name)),
    )
    .unwrap_or_default();
    let inputs = pp::Inputs {
        pkg: &ctx.config.rpm_name,
        spec: &spec,
        changes: &changes,
        version: &ctx.config.version,
        repo: crate::bugfix::upstream_repo_of_spec(&spec),
        patches: &patches,
    };
    let provenance = pp::assess(&inputs, net, crate::upstream_cache::refreshing());
    for p in &provenance {
        let text = patches
            .iter()
            .find(|(n, _)| *n == p.name)
            .map(|(_, t)| t.as_str())
            .unwrap_or_default();
        let url = p
            .origin
            .as_ref()
            .map(|o| o.url.as_str())
            .unwrap_or_default();
        match &p.verdict {
            Verdict::HeadMoved { .. } => {
                if replace_with_revision(ctx, st, step_id, check, review, net, p, text, lines)?
                    == Replaced::Taken
                {
                    let bytes = std::fs::read(ctx.checkout_dir.join(&p.name)).unwrap_or_default();
                    accepted.insert(p.name.clone(), bytes);
                    refreshed.insert(p.name.clone());
                }
            }
            Verdict::Redundant { tag } => {
                let mut notes = Vec::new();
                drop_merged(ctx, &mut spec, &p.name, url, tag, &mut notes);
                lines.extend(notes);
            }
            Verdict::ReleasedLater { tag, .. } => lines.push(format!(
                "- {}: the forge says {url} was merged in {tag}; prove a removal or rebase it",
                p.name
            )),
            _ => {}
        }
    }
    Ok(())
}
