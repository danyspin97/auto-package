//! The memo of a spent resolution ladder.

use super::*;
use crate::pipeline::ladder::LADDER_FAILED_PREFIX;
use crate::pipeline::ladder_end::{REASON_CAP, force_hint, memo_head, then_step};
use crate::pipeline::ladder_recap::recap;
use crate::pipeline::sibling::*;
use ap_core::failure_note::FailureNote;

/// The key a failed ladder is remembered under: the spec, and the specs of the
/// packages it depends on, so a sibling's patch counts as changed input.
pub(super) fn ladder_key(config: &PipelineConfig, spec: &str) -> u64 {
    // The memo is written from the stripped spec the agent edited, and the
    // check reads the spec on disk, which may still carry `# ap: fix` lines the
    // agent kept: both sides hash the same stripped text.
    let mut all = crate::spec::strip_ap_fix_comments(spec);
    for dir in sibling_checkouts(config, spec) {
        if let Some(name) = dir.file_name() {
            all.push_str(
                &std::fs::read_to_string(dir.join(format!("{}.spec", name.to_string_lossy())))
                    .unwrap_or_default(),
            );
        }
    }
    hash_spec(&all)
}

/// Whether an exhausted ladder is remembered (its spec and reason written as the
/// memo `spent_ladder_refusal` reads). A patch-driven ladder is not, and neither
/// is one the budget stopped, since its rungs never ran.
pub(super) fn should_remember_ladder(patch_task: bool, budget_stopped: bool) -> bool {
    !patch_task && !budget_stopped
}

/// The recap of a stored history, without the typed remedy and the resolver's
/// own words.
fn recap_of(history: &[String]) -> crate::pipeline::ladder_recap::Recap {
    recap(history, None, None, None, ap_core::remedy::host_distro())
}

/// The history a memo written before the recap was kept stands for: its
/// `turn 0: … | round 1: … | round 2: …` text, without the wrapper of the
/// refusal it was once shown in, one entry per turn and round.
fn legacy_history(full: &str) -> Vec<String> {
    const WRAPPER: &str = "neither it nor the packages it depends on changed (";
    const TAIL: &str = "; not trying again without --force";
    let mut text = full.trim();
    if let Some(at) = text.find("turn 0:") {
        text = &text[at..];
    }
    if let Some(at) = text.rfind(TAIL) {
        text = &text[..at];
        if full.contains(WRAPPER) {
            text = text.strip_suffix(')').unwrap_or(text);
        }
    }
    let mut parts = text.split(" | round ");
    let mut history: Vec<String> = parts.next().map(str::to_string).into_iter().collect();
    history.extend(parts.map(|p| format!("round {p}")));
    history
}

/// The `reason` and the `action` of the refusal to run a spent ladder again:
/// `summary` is the stored recap, `action` the stored step, `pkg` the package
/// as the user types it. A rerun needs `--force` once the cause is fixed.
pub(super) fn repeat_failure(
    rpm: &str,
    pkg: &str,
    summary: &str,
    action: Option<&str>,
) -> (String, String) {
    let reason = format!("{} {summary}", memo_head(rpm));
    (
        crate::pipeline::ladder_recap::cap(reason.trim_end(), REASON_CAP),
        then_step(action, &force_hint(pkg)),
    )
}

/// The failure for a spec whose resolution ladder was already spent: the same
/// ladder would end the same way, at the price of several turns, while the spec
/// and the specs it depends on are unchanged. `--force` and a package that is
/// being patched for a dependent try afresh.
pub(super) fn spent_ladder_refusal(
    ctx: &RunCtx,
    patching: bool,
    spec_text: &str,
) -> Option<PipelineResult> {
    if patching || ctx.config.force {
        return None;
    }
    let state = PkgState::load(&ctx.ws);
    if state.ladder_spec_hash != Some(ladder_key(ctx.config, spec_text)) {
        return None;
    }
    let full = state.ladder_reason.unwrap_or_default();
    let kind = if state.ladder_needs_maintainer {
        FailureKind::NeedsMaintainer
    } else {
        FailureKind::Exhausted
    };
    let recap = state.ladder_recap;
    let (summary, action) = match recap.ladder_summary {
        Some(summary) => (summary, recap.ladder_action),
        // A memo written before the recap was kept: recap its text.
        None => {
            let old = recap_of(&legacy_history(&full));
            (
                old.body_within(
                    REASON_CAP.saturating_sub(memo_head(&ctx.rpm_name).chars().count() + 1),
                    0,
                ),
                None,
            )
        }
    };
    let (reason, action) = repeat_failure(
        &ctx.rpm_name,
        ctx.config
            .request_text
            .as_deref()
            .unwrap_or(&ctx.config.pkg_name),
        &summary,
        action.as_deref(),
    );
    let detail = format!(
        "{LADDER_FAILED_PREFIX}{}: the resolution ladder was spent on this spec earlier and \
         neither it nor the packages it depends on changed ({full}); not trying again \
         without --force",
        ctx.rpm_name
    );
    Some(ctx.failed_note(
        kind,
        reason,
        FailureNote {
            detail: Some(detail),
            action: Some(action),
        },
    ))
}
