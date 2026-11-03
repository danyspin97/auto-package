//! The review of the patches a fix turn wrote or edited outside the patch stage:
//! the turn's edit is kept only when every such patch is accepted.

use super::*;
use crate::pipeline::patch_review::*;
use crate::pipeline::patch_rewrite::code_changed;
use crate::pipeline::patch_rules::{PatchStatus as Judged, judge_patches};
use crate::pipeline::rejection::note_spec_rejection;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// A patch a turn made or edited: its name, its bytes before (`None` for a new
/// patch) and its level.
struct Edited {
    name: String,
    original: Option<Vec<u8>>,
}

/// The patches the turn rewrote: new, or declared with different code. A
/// header-only or offset-only edit is not one.
fn rewritten_by_turn(spec_path: &Path, before: &str, snapshot: &CheckoutSnapshot) -> Vec<Edited> {
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    let mut out: Vec<Edited> = Vec::new();
    for judged in judge_patches(spec_path, before, &after, Some(snapshot)) {
        let Some(now) = &judged.bytes else { continue };
        let original = match judged.status {
            Judged::New => None,
            Judged::Changed(old) => old,
        };
        let level = crate::patch_check::declared_patches(&after)
            .into_iter()
            .find(|(_, n)| *n == judged.name)
            .map_or(1, |(number, _)| spec_patch_level(&after, &number));
        if code_changed(original.as_deref(), now, level)
            && !out.iter().any(|e| e.name == judged.name)
        {
            out.push(Edited {
                name: judged.name,
                original,
            });
        }
    }
    out
}

/// [`review_turn_patches`] with the review turn given.
#[allow(clippy::too_many_arguments)]
pub(super) fn review_turn_patches_with(
    ctx: &RunCtx,
    st: &mut RunState,
    step: &StepDir,
    snapshot: &CheckoutSnapshot,
    before: &str,
    label: &str,
    effect: TurnEffect,
    applies: &mut ApplyCheck<'_>,
    review: &mut ReviewTurn<'_>,
) -> Result<TurnEffect, PipelineResult> {
    if effect != TurnEffect::Edited {
        settle_record(&ctx.ws, &ctx.spec_path);
        return Ok(effect);
    }
    let (reporter, spec_path) = (ctx.reporter, &ctx.spec_path);
    let rewritten = rewritten_by_turn(spec_path, before, snapshot);
    let mut lines: Vec<String> = Vec::new();
    // Marked only once the turn is accepted as a whole: a later patch may reject.
    let mut rewrote_names: Vec<String> = Vec::new();
    // The turn's edit goes back as a whole; `why` is told to its next turn.
    let roll_back = |why: &str| {
        snapshot.restore(reporter, Some(step));
        restore_spec(spec_path, before, Some(step), reporter);
        note_spec_rejection(spec_path, why);
        append_notes(
            ctx.ws.notes(),
            &format!("\n## {label} — rolled back by the patch review\n{why}\n"),
        );
    };
    for Edited { name, original } in rewritten {
        let cause = if original.is_some() {
            format!("edited by a {label} turn")
        } else {
            format!("added by a {label} turn")
        };
        let end = review_patch_rounds(
            ctx,
            st,
            step.id,
            &name,
            original.as_deref(),
            &cause,
            applies,
            review,
            &mut lines,
        );
        let stop = |why: String| {
            roll_back(&why);
            ctx.failed_as(FailureKind::NeedsMaintainer, why)
        };
        match end {
            Err(failed) => {
                roll_back(&format!("the patch review of {name} did not finish"));
                return Err(failed);
            }
            Ok(PatchReviewEnd::Accepted { rewrote }) => {
                if rewrote {
                    rewrote_names.push(name.clone());
                }
            }
            Ok(PatchReviewEnd::Rejected(why)) => {
                roll_back(&format!("the patch review rejected {name}: {why}"));
                // A rewrite that could not be put back stays recorded, and stops.
                if !settle_record(&ctx.ws, &ctx.spec_path) {
                    return Err(ctx.failed_as(
                        FailureKind::NeedsMaintainer,
                        format!(
                            "the rejected rewrite of {name} could not be rolled back; the patches \
                             as they were are in {}",
                            ctx.ws.dir().join("patch-review/start").display()
                        ),
                    ));
                }
                return Ok(TurnEffect::NoChange);
            }
            Ok(PatchReviewEnd::GaveUp(why)) => {
                return Err(stop(format!(
                    "the patch review gave up on {name}: {why}; it needs a maintainer"
                )));
            }
            Ok(PatchReviewEnd::Unsettled(why)) => {
                return Err(stop(format!(
                    "the patch review did not settle {name} in {MAX_REVIEW_ROUNDS} rounds \
                     ({why}); it needs a maintainer"
                )));
            }
        }
    }
    for name in &rewrote_names {
        if let Err(why) = mark_reviewer_rewrite(&ctx.ws, &ctx.checkout_dir, name) {
            roll_back(&why);
            return Err(ctx.failed_as(FailureKind::NeedsMaintainer, why));
        }
    }
    end_turn(&ctx.ws);
    Ok(TurnEffect::Edited)
}

/// Whether the patch `name` applies to the pristine sources, by the spec's own
/// check (`patch -p1 --fuzz=0`, each patch alone). Fails closed: a patch with
/// another level, a check that could not run and a patch it could not check are
/// all `Err`. The other patches' results do not count.
pub(super) fn applies_to_pristine(ctx: &RunCtx, name: &str) -> Result<(), String> {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    // `try_patches` says nothing about a patch it does not run on (no
    // `%autosetup`, a name with a directory, another level): only a patch it
    // positively covers is proven.
    if !spec
        .lines()
        .any(|l| !l.trim_start().starts_with('#') && l.contains("%autosetup"))
    {
        return Err("the spec has no %autosetup, so the patch check does not run".into());
    }
    if std::path::Path::new(name).file_name() != Some(std::ffi::OsStr::new(name)) {
        return Err("its name is not a bare file name, which the patch check skips".into());
    }
    let Some((number, _)) = crate::patch_check::declared_patches(&spec)
        .into_iter()
        .find(|(_, n)| n == name)
    else {
        return Err("the spec does not declare it".into());
    };
    let level = spec_patch_level(&spec, &number);
    if level != 1 {
        return Err(format!(
            "it applies with -p{level}, which the check does not cover"
        ));
    }
    // Without the source archive nothing proves the patch applies.
    let version = crate::spec::parse_header(&spec)
        .map(|h| h.version)
        .unwrap_or_default();
    if find_tarball(&ctx.checkout_dir, &version).is_none() {
        return Err("there is no source archive to check it against".into());
    }
    let check = try_patches(&ctx.spec_path, &spec)
        .map_err(|e| format!("the patch check could not run: {e}"))?;
    if let Some((_, detail)) = check.failed.iter().find(|(n, _)| n == name) {
        return Err(format!(
            "{name} does not apply with `patch -p1 --fuzz=0`: {detail}"
        ));
    }
    match &check.unchecked {
        Some(note) if note.contains(name) || !note.contains(".patch") => {
            Err(format!("it could not be checked: {note}"))
        }
        _ => Ok(()),
    }
}

/// Reviews the patches the fix turn `step` (labelled `label`) wrote or edited,
/// after its edit was accepted (`effect`). `snapshot` and `before` are the
/// checkout and the spec before the turn. A rejected patch rolls the turn back
/// and the site goes on as for `NoChange`; a patch the reviewer gives up on
/// stops the package for a maintainer.
pub(super) fn review_turn_patches(
    ctx: &RunCtx,
    st: &mut RunState,
    step: &StepDir,
    snapshot: &CheckoutSnapshot,
    before: &str,
    label: &str,
    effect: TurnEffect,
) -> Result<TurnEffect, PipelineResult> {
    review_turn_patches_with(
        ctx,
        st,
        step,
        snapshot,
        before,
        label,
        effect,
        &mut applies_to_pristine,
        &mut agent_review_turn,
    )
}
