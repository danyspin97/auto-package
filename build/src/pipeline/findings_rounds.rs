//! The turn rounds of the findings stage: the findings routed to the agent go
//! to a FixFindings turn, for at most [`MAX_FINDINGS_ROUNDS`] rounds, each
//! through the acceptance gate and the patch review of any turn's edit.

use super::*;
use crate::agent::{Action, FindingAnswer};
use crate::bugfix::{FindingsJob, ReviewFinding, apply_changes_entry};
use crate::pipeline::apply_turn::*;
use crate::pipeline::bug_fix_rounds::FixTurn;
mod prompt;
use crate::pipeline::findings_stage::*;
use crate::pipeline::patch_review::{ApplyCheck, ReviewTurn};
use crate::pipeline::patch_review_edit::review_turn_patches_with;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;
use prompt::findings_context;

/// Rounds of FixFindings turns a request gets.
pub(super) const MAX_FINDINGS_ROUNDS: usize = 3;

/// The production [`FixTurn`] of the findings stage.
pub(super) fn agent_findings_turn(
    ctx: &RunCtx,
    st: &mut RunState,
    round: usize,
    context: &str,
) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
    agent_turn(
        TurnCtx {
            carried: false,
            label: "fix-findings",
            task: crate::agent::Task::FixFindings,
            event_task: ap_core::event::Task::FixFindings,
            attempt: st.attempts,
            hint: None,
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            checkout_dir: Some(&ctx.checkout_dir),
            from_repo_archive: st.from_repo_archive,
            resume: round > 1,
            resolver: false,
            context: Some(context),
        },
        ctx.turn_env(),
        &mut st.spent_usd,
        &mut st.session_id,
        &mut st.step_attempts,
    )
}

/// What the stage calls out to for the turn rounds.
pub(super) struct TurnSeams<'a> {
    pub(super) turn: &'a mut FixTurn<'a>,
    pub(super) applies: &'a mut ApplyCheck<'a>,
    pub(super) review: &'a mut ReviewTurn<'a>,
}

/// What a turn's answer means for the stage.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FindingsMove {
    /// The edit was accepted.
    Done,
    /// Another round, told why; the edit is rolled back.
    Retry(String),
    /// The turn changed nothing; its answers say why.
    Nothing,
    /// A maintainer decides.
    Maintainer(String),
}

pub(super) fn findings_move(
    outcome: &crate::agent::TurnOutcome,
    effect: &Result<TurnEffect, TurnError>,
    rejected: Option<&str>,
) -> FindingsMove {
    if let Action::Blocked { why } = &outcome.action {
        let why = if why.is_empty() {
            "the turn blocked without a reason"
        } else {
            why
        };
        return FindingsMove::Maintainer(why.to_string());
    }
    if let Err(e) = effect {
        return FindingsMove::Maintainer(e.reason.clone());
    }
    if outcome.finding_answers.is_empty() {
        return FindingsMove::Retry("answer with the `finding_answers` field".into());
    }
    match (&outcome.action, matches!(effect, Ok(TurnEffect::Edited))) {
        (Action::Edited, true) => FindingsMove::Done,
        (Action::Edited, false) => {
            FindingsMove::Retry(rejected.unwrap_or("the gate refused your edit").to_string())
        }
        _ => FindingsMove::Nothing,
    }
}

/// What the rounds ended with.
pub(super) struct RoundsEnd {
    /// A turn's edit was accepted.
    pub(super) edited: bool,
    pub(super) answers: Vec<FindingAnswer>,
}

/// Writes `bullets` as the bullets of the request's own newest entry.
fn write_entry(
    job: &FindingsJob,
    checkout: &Path,
    spec: &str,
    bullets: &str,
) -> Result<(), String> {
    let path = osc::newest_changes_file(checkout).ok_or("the package has no .changes file")?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let patches: Vec<String> = crate::patch_check::declared_patches(spec)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let fixed = apply_changes_entry(&text, job.target_changes.as_deref(), bullets, &patches)?;
    ap_core::fsx::write_atomic(&path, fixed)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The turn rounds for `findings`, which the tool could not fix.
pub(super) fn findings_rounds(
    fs: &Findings<'_>,
    st: &mut RunState,
    seams: &mut TurnSeams<'_>,
    findings: &[&ReviewFinding],
) -> Result<RoundsEnd, PipelineResult> {
    let (ctx, job) = (fs.ctx, fs.job);
    let mut feedback: Option<String> = None;
    let mut last = String::new();
    for round in 1..=MAX_FINDINGS_ROUNDS {
        if crate::proc::shutting_down() {
            let why = "stopped while fixing the findings: the run is shutting down".to_string();
            return Err(fs.stop("interrupted", FailureKind::Transient, why));
        }
        if let Err(why) = ctx.may_start_turn(st.spent_usd) {
            return Err(fs.stop("exhausted", FailureKind::Exhausted, why));
        }
        st.attempts += 1;
        persist_attempts(&ctx.ws, &ctx.config.run_id, st.attempts);
        let before_spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
        // Dropped at the end of the round: the file is never left behind.
        let (_series, _) = write_rebase_series(&before_spec, &ctx.checkout_dir);
        pop_source_patches(ctx);
        let snapshot = CheckoutSnapshot::take(&ctx.checkout_dir);
        let context = findings_context(
            job,
            round,
            MAX_FINDINGS_ROUNDS,
            feedback.as_deref(),
            findings,
        );
        let reply = (seams.turn)(ctx, st, round, &context);
        pop_source_patches(ctx);
        let (turn_step, outcome) = match reply {
            Ok(v) => v,
            Err((e, kind)) => {
                let why = format!("the findings turn failed: {e}");
                let verdict = match kind {
                    FailureKind::NeedsMaintainer => "needs_maintainer",
                    FailureKind::RateLimited
                    | FailureKind::AgentUnavailable
                    | FailureKind::Exhausted => "exhausted",
                    _ => "failed",
                };
                return Err(fs.stop(verdict, kind, why));
            }
        };
        let roll_back = || {
            snapshot.restore(ctx.reporter, Some(&turn_step));
            restore_spec(&ctx.spec_path, &before_spec, Some(&turn_step), ctx.reporter);
        };
        if matches!(outcome.action, Action::UseUpstreamWheel { .. }) {
            roll_back();
            last =
                "use_upstream_wheel is not a fix of a finding: fix the findings or answer".into();
            feedback = Some(last.clone());
            continue;
        }
        let mut rejected: Option<String> = None;
        let effect = apply_turn_and_reset(
            ApplyTurnCtx {
                config: ctx.config,
                checkout_dir: &ctx.checkout_dir,
                spec_path: &ctx.spec_path,
                step: &turn_step,
                before: &before_spec,
                rpm_name: &ctx.rpm_name,
                label: "FixFindings",
                attempt: round,
                notes_path: ctx.ws.notes(),
                snapshot: Some(&snapshot),
                kept: &[],
                staged_out: None,
                rejected_out: Some(&mut rejected),
            },
            &outcome,
            ctx.reporter,
            &mut st.sdist_top_level,
            &mut st.from_repo_archive,
        );
        pop_source_patches(ctx);
        match findings_move(&outcome, &effect, rejected.as_deref()) {
            FindingsMove::Maintainer(why) => {
                return Err(fs.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
            }
            FindingsMove::Nothing => {
                return Ok(RoundsEnd {
                    edited: false,
                    answers: outcome.finding_answers,
                });
            }
            FindingsMove::Retry(why) => {
                roll_back();
                last = why.clone();
                feedback = Some(why);
            }
            FindingsMove::Done => {
                let spec_now = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
                let version_now = crate::spec::parse_header(&spec_now).map(|h| h.version);
                let refusal = if version_now.as_deref() != Some(job.version.as_str()) {
                    Some("`Version:` must stay as it is".to_string())
                } else if outcome.changes_entry.trim().is_empty() {
                    None
                } else {
                    write_entry(job, &ctx.checkout_dir, &spec_now, &outcome.changes_entry).err()
                };
                if let Some(why) = refusal {
                    roll_back();
                    last = why.clone();
                    feedback = Some(why);
                    continue;
                }
                match review_turn_patches_with(
                    ctx,
                    st,
                    &turn_step,
                    &snapshot,
                    &before_spec,
                    "FixFindings",
                    TurnEffect::Edited,
                    seams.applies,
                    seams.review,
                )? {
                    TurnEffect::Edited => {
                        return Ok(RoundsEnd {
                            edited: true,
                            answers: outcome.finding_answers,
                        });
                    }
                    _ => {
                        last = "the patch review rejected your edit".to_string();
                        feedback = Some(last.clone());
                    }
                }
            }
        }
    }
    let why = if last.is_empty() {
        "the findings turn did not settle the findings".to_string()
    } else {
        last
    };
    Err(fs.stop(
        "needs_maintainer",
        FailureKind::NeedsMaintainer,
        format!("{why} (after {MAX_FINDINGS_ROUNDS} rounds)"),
    ))
}
