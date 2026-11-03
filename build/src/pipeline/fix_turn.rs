//! One fix turn: the agent is asked for an edit to the spec, the edit is
//! applied through the gate and the journal, and the effect is returned. Used
//! by [`fix_after_build`](super::fix_after::fix_after_build) after a failed
//! build and by the rpmlint round after a green one.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::apply_turn::*;
use crate::pipeline::ladder::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// Run one fix turn as attempt `attempts` (counted by the caller) of `task`
/// with the given hint and context, and apply what the agent did. `build_log`
/// is the build's log when there is one.
pub(super) fn fix_turn(
    ctx: &RunCtx,
    st: &mut RunState,
    attempts: usize,
    task: crate::agent::Task,
    hint: Option<&crate::log::ExtractedLog>,
    context: Option<&str>,
    build_log: Option<&Path>,
) -> Result<TurnEffect, PipelineResult> {
    let RunCtx {
        config,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;

    let clean = strip_and_rewrite(spec_path);
    let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);
    let turn = agent_turn(
        TurnCtx {
            carried: false,
            label: "fix-after-build",
            task,
            event_task: ap_core::event::Task::FixAfterBuild {
                attempt: attempts as u32,
                max_attempts: config.max_attempts as u32,
            },
            attempt: attempts,
            hint,
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: true,
            context,
        },
        ctx.turn_env(),
        &mut st.spent_usd,
        &mut st.session_id,
        &mut st.step_attempts,
    );

    let (agent_step, outcome) = match turn {
        Ok(v) => v,
        // `agent_turn` already routed the Error through
        // `friendly_agent_error`, so this is a user-ready message
        // (budget-exhausted hint included); pass it through verbatim.
        Err((e, kind)) => {
            discard_failed_turn(
                ws.notes(),
                reporter,
                &checkout_snapshot,
                spec_path,
                &clean,
                "FixAfterBuild",
            );
            return Err(ctx.failed_as(kind, e));
        }
    };

    if let Some(missing) = missing_deps_from_turn(ctx, agent_step.id, &outcome) {
        // This path skips the apply gate, so the turn's edit would reach the
        // next build unvalidated and unjournaled. Roll back to the state the
        // turn started from, and journal the discarded turn.
        checkout_snapshot.restore(reporter, Some(&agent_step));
        restore_spec(spec_path, &clean, Some(&agent_step), reporter);
        append_notes(
            ws.notes(),
            &format!(
                "\n## Attempt {attempts} — FixAfterBuild — missing dependencies; edits discarded\nReasoning: {}\n",
                brief(&outcome.reasoning)
            ),
        );
        return Err(missing);
    }
    // Everything the turn called missing is already built here: its edit
    // is applied below and the build runs again.

    let mut staged: Vec<String> = Vec::new();
    let effect = match apply_turn_and_reset(
        ApplyTurnCtx {
            config,
            checkout_dir,
            spec_path,
            step: &agent_step,
            before: &clean,
            rpm_name,
            label: "FixAfterBuild",
            attempt: attempts,
            notes_path: ws.notes(),
            snapshot: Some(&checkout_snapshot),
            kept: &[],
            staged_out: Some(&mut staged),
            rejected_out: None,
        },
        &outcome,
        reporter,
        &mut st.sdist_top_level,
        &mut st.from_repo_archive,
    ) {
        Ok(effect) => effect,
        Err(reason) => resolve_blocked(
            ctx,
            st,
            LadderSite {
                build_log,
                staged: &staged,
                task,
                event_task: ap_core::event::Task::FixAfterBuild {
                    attempt: attempts as u32,
                    max_attempts: config.max_attempts as u32,
                },
                attempt: attempts,
                hint,
                context,
                apply_label: "FixAfterBuild",
                before: &clean,
                snapshot: &checkout_snapshot,
                step: &agent_step,
            },
            &outcome,
            reason,
        )?,
    };
    let effect = review_turn_patches(
        ctx,
        st,
        &agent_step,
        &checkout_snapshot,
        &clean,
        "FixAfterBuild",
        effect,
    )?;
    // A dependency the fix added is queued now, not after another build OBS
    // rejects as unresolvable.
    if effect == TurnEffect::Edited
        && let Some(missing) = missing_deps_added_by_edit(ctx, agent_step.id, &clean)
    {
        return Err(missing);
    }

    Ok(effect)
}
