//! The turn rounds of the bug fix stage: when the mechanical path cannot add
//! the upstream patch, or there is none, a FixBug turn does it, for at most
//! [`MAX_FIX_ROUNDS`] rounds, each through the acceptance gate of any turn's edit.

use super::*;
use crate::agent::{Action, FixVerdict};
use crate::bugfix::{fix_patch_name, patch_prefix};
use crate::pipeline::apply_turn::*;
use crate::pipeline::bug_fix_check::check_fix;
use crate::pipeline::bug_fix_prompt::*;
use crate::pipeline::bug_fix_stage::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// Rounds of FixBug turns a fix gets.
pub(super) const MAX_FIX_ROUNDS: usize = 3;

/// Runs one FixBug turn, given the round and the Context.
pub(super) type FixTurn<'a> = dyn FnMut(
        &RunCtx,
        &mut RunState,
        usize,
        &str,
    ) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)>
    + 'a;

/// The production [`FixTurn`].
pub(super) fn agent_fix_turn(
    ctx: &RunCtx,
    st: &mut RunState,
    round: usize,
    context: &str,
) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)> {
    agent_turn(
        TurnCtx {
            carried: false,
            label: "fix-bug",
            task: crate::agent::Task::FixBug,
            event_task: ap_core::event::Task::FixBug,
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

/// What the first turn is told about the staged patch the stage could not add.
#[derive(Debug, Clone, Default)]
pub(super) struct Retry {
    failure: Option<String>,
    rejects: String,
}

impl Retry {
    /// Nothing was staged, or nothing failed.
    pub(super) fn none() -> Retry {
        Retry::default()
    }

    /// The stage could not edit or gate the spec.
    pub(super) fn spec(why: String) -> Retry {
        Retry {
            failure: Some(why),
            rejects: String::new(),
        }
    }

    /// Quilt rejected hunks of `name`.
    pub(super) fn quilt(name: &str, reason: String, rejects: String) -> Retry {
        Retry {
            failure: Some(format!("{name} does not apply: {reason}")),
            rejects,
        }
    }

    pub(super) fn why(&self) -> String {
        self.failure.clone().unwrap_or_default()
    }
}

/// What a turn's answer means for the stage.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FixMove {
    /// The edit was accepted and the answer says fixed: check the fix.
    PostChecks,
    /// Another round, told why; the edit is rolled back.
    Retry(String),
    /// The bug needs no fix here: `verdict` is `already_fixed`, `not_affected`
    /// or `no_fix`.
    NoFix {
        verdict: &'static str,
        reason: String,
    },
    /// A maintainer decides.
    Maintainer(String),
}

/// The move a turn's answer makes, given what the gate did with its edit.
/// `rejected` is the gate's reason when it refused the edit.
pub(super) fn fix_move(
    outcome: &crate::agent::TurnOutcome,
    effect: &Result<TurnEffect, TurnError>,
    rejected: Option<&str>,
) -> FixMove {
    if let Action::Blocked { why } = &outcome.action {
        let why = [
            why.as_str(),
            outcome.bug_fix.as_ref().map_or("", |f| f.reason.as_str()),
        ]
        .into_iter()
        .find(|w| !w.is_empty())
        .unwrap_or("the turn blocked without a reason");
        return FixMove::Maintainer(why.to_string());
    }
    let Some(fix) = &outcome.bug_fix else {
        return FixMove::Retry("answer with the `fix` field".into());
    };
    if let Err(e) = effect {
        return FixMove::Maintainer(e.reason.clone());
    }
    let edited = matches!(effect, Ok(TurnEffect::Edited));
    let said = |v: &str| {
        FixMove::Retry(format!(
            "you edited but answered {v}: a fix is an edit and an answer of `fixed`"
        ))
    };
    match fix.verdict {
        FixVerdict::Fixed => match (&outcome.action, edited) {
            (Action::Edited, true) => FixMove::PostChecks,
            (Action::Edited, false) => {
                FixMove::Retry(rejected.unwrap_or("the gate refused your edit").to_string())
            }
            _ => FixMove::Retry("you answered fixed but changed nothing".into()),
        },
        FixVerdict::AlreadyFixed | FixVerdict::NotAffected => {
            let v = if fix.verdict == FixVerdict::AlreadyFixed {
                "already_fixed"
            } else {
                "not_affected"
            };
            if edited {
                said(v)
            } else if fix.evidence.iter().all(|e| e.trim().is_empty()) {
                FixMove::Retry(format!(
                    "you answered {v} without evidence: quote the code that shows it in `evidence`"
                ))
            } else {
                FixMove::NoFix {
                    verdict: if v == "already_fixed" {
                        "already_fixed"
                    } else {
                        "not_affected"
                    },
                    reason: fix.reason.clone(),
                }
            }
        }
        FixVerdict::NoFix if edited => said("no_fix"),
        FixVerdict::NoFix => FixMove::NoFix {
            verdict: "no_fix",
            reason: fix.reason.clone(),
        },
        FixVerdict::NeedsMaintainer => FixMove::Maintainer(fix.reason.clone()),
    }
}

/// One line of at most `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        line
    } else {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

/// The turn rounds. `first` is what the mechanical path could not do.
pub(super) fn fix_rounds(
    fix: Fix<'_>,
    st: &mut RunState,
    mut seams: FixSeams<'_>,
    first: Retry,
) -> Result<(), PipelineResult> {
    let (ctx, job) = (fix.ctx, fix.job);
    let staged = job.patches.first();
    let patch_name = staged.map_or_else(
        || {
            fix_patch_name(
                &patch_prefix(&job.pkg, &fix.baseline),
                &job.refs,
                None,
                0,
                1,
            )
        },
        |p| p.file_name.clone(),
    );
    let mut feedback: Option<String> = None;
    let mut last = String::new();
    for round in 1..=MAX_FIX_ROUNDS {
        if crate::proc::shutting_down() {
            let why = "stopped while fixing the bug: the run is shutting down".to_string();
            // The staged patch and the spec edit go back, as for any stop.
            return Err(fix.stop("interrupted", FailureKind::Transient, why));
        }
        if let Err(why) = ctx.may_start_turn(st.spent_usd) {
            return Err(fix.stop("exhausted", FailureKind::Exhausted, why));
        }
        st.attempts += 1;
        persist_attempts(&ctx.ws, &ctx.config.run_id, st.attempts);
        let before_spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
        // Dropped at the end of the round: the file is never left behind.
        let (_series, _) = write_rebase_series(&before_spec, &ctx.checkout_dir);
        pop_source_patches(ctx);
        let snapshot = CheckoutSnapshot::take(&ctx.checkout_dir);
        let context = fix_bug_context(&FixContextInput {
            round,
            max_rounds: MAX_FIX_ROUNDS,
            refusal: feedback.as_deref(),
            job,
            version: &ctx.config.version,
            patch_name: &patch_name,
            staged: staged.map(|p| StagedInfo {
                patch: p,
                comment: crate::backport::spec_comment(p, &job.refs),
                failure: first.failure.as_deref().filter(|_| round == 1),
                rejects: if round == 1 { &first.rejects } else { "" },
            }),
        });
        let reply = (seams.turn)(ctx, st, round, &context);
        pop_source_patches(ctx);
        let (turn_step, outcome) = match reply {
            Ok(v) => v,
            Err((e, kind)) => {
                let why = format!("the fix turn failed: {e}");
                return Err(fix.stop(turn_failure_verdict(kind), kind, why));
            }
        };
        let roll_back = || {
            snapshot.restore(ctx.reporter, Some(&turn_step));
            restore_spec(&ctx.spec_path, &before_spec, Some(&turn_step), ctx.reporter);
        };
        // A wheel switch replaces the whole recipe: it is no answer to a bug.
        if matches!(outcome.action, Action::UseUpstreamWheel { .. }) {
            roll_back();
            feedback = Some("use_upstream_wheel is not a bug fix: fix the bug or answer".into());
            last = feedback.clone().unwrap_or_default();
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
                label: "FixBug",
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
        match fix_move(&outcome, &effect, rejected.as_deref()) {
            FixMove::Maintainer(why) => {
                return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
            }
            FixMove::NoFix { verdict, reason } => {
                return Err(fix.stop(verdict, FailureKind::NoFix, reason));
            }
            FixMove::Retry(why) => {
                roll_back();
                last = why.clone();
                feedback = Some(why);
            }
            FixMove::PostChecks => {
                let done = match found_fix(&fix, &outcome, &ctx.checkout_dir) {
                    Ok(done) => done,
                    Err(why) => {
                        roll_back();
                        last = why.clone();
                        feedback = Some(why);
                        continue;
                    }
                };
                match settle(&fix, st, &mut seams, &done, true) {
                    Ok(()) => {
                        fix.step.finish(StepOutcome::Ok);
                        return Ok(());
                    }
                    Err(Settle::Stop(result)) => return Err(result),
                    Err(Settle::Retry(why)) => {
                        roll_back();
                        last = why.clone();
                        feedback = Some(why);
                    }
                }
            }
        }
    }
    let (verdict, kind) = if staged.is_some() {
        ("does_not_apply", FailureKind::FixDoesNotApply)
    } else {
        ("no_fix", FailureKind::NoFix)
    };
    let why = if last.is_empty() {
        "the fix turn did not settle the fix".to_string()
    } else {
        last
    };
    Err(fix.stop(
        verdict,
        kind,
        format!("{why} (after {MAX_FIX_ROUNDS} rounds)"),
    ))
}

/// The patch an accepted `fixed` turn left as the fix: the staged one, or the
/// one the spec now declares that it did not.
fn found_fix(
    fix: &Fix<'_>,
    outcome: &crate::agent::TurnOutcome,
    checkout: &Path,
) -> Result<Done, String> {
    let job = fix.job;
    let spec = std::fs::read_to_string(&fix.ctx.spec_path).unwrap_or_default();
    if !job.patches.is_empty() {
        let mut patches = Vec::new();
        let mut adapted = false;
        for p in &job.patches {
            adapted |= std::fs::read(checkout.join(&p.file_name)).ok().as_deref() != Some(&p.bytes);
            patches.push(Fixed::staged(
                p,
                Some(p.bytes.clone()),
                format!(
                    "the upstream fix {}, staged by the tool{}",
                    p.origin,
                    if adapted {
                        " and adapted by a bug fix turn"
                    } else {
                        ""
                    }
                ),
            ));
        }
        return Ok(Done {
            patches,
            applied: if adapted { "adapted" } else { "clean" },
        });
    }
    let name = check_fix(&spec, checkout, &job.refs, None, &fix.baseline)?;
    let bytes = std::fs::read(checkout.join(&name)).unwrap_or_default();
    let answer = outcome.bug_fix.as_ref();
    let upstream = answer.map_or("", |a| a.upstream.trim());
    let unreported = String::from_utf8_lossy(&bytes)
        .to_ascii_lowercase()
        .contains("upstream: not yet reported");
    let reason = one_line(answer.map_or("", |a| a.reason.as_str()), 120);
    let reason = if reason.is_empty() {
        "fix the reported bug".to_string()
    } else {
        crate::bugfix::spec_safe(&reason)
    };
    let patch = crate::backport::PreparedPatch {
        file_name: name,
        bytes,
        origin: if upstream.is_empty() {
            "the fix turn".to_string()
        } else {
            upstream.to_string()
        },
        commit: None,
        reason,
    };
    let cause = if unreported {
        "a bug fix turn wrote it: there is no upstream fix".to_string()
    } else {
        format!(
            "a bug fix turn made it from the upstream fix {}",
            patch.origin
        )
    };
    Ok(Done {
        patches: vec![Fixed {
            patch,
            original: None,
            cause,
        }],
        applied: if unreported { "written" } else { "adapted" },
    })
}
