//! The memo of a spent resolution ladder.

use super::*;
use crate::pipeline::ladder::LADDER_FAILED_PREFIX;
use crate::pipeline::sibling::*;

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
    let reason = state.ladder_reason.unwrap_or_default();
    let kind = if state.ladder_needs_maintainer {
        FailureKind::NeedsMaintainer
    } else {
        FailureKind::Exhausted
    };
    Some(ctx.failed_as(
        kind,
        format!(
            "{LADDER_FAILED_PREFIX}{}: the resolution ladder was spent on this spec earlier and \
             neither it nor the packages it depends on changed ({reason}); not trying again \
             without --force",
            ctx.rpm_name
        ),
    ))
}
