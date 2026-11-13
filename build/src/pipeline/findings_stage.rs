//! The findings stage of `auto-package fix --request`: the pre-build stage of
//! a run in place of the review. It makes the tool fixes of the review's
//! findings without a model, then has a FixFindings turn fix the rest that
//! routes to the agent, each edit through the acceptance gate and the patch
//! review of any turn. The ordinary stages follow: validate, commit, build.

use std::collections::HashMap;

use super::*;
use crate::agent::FindingStatus;
use crate::bugfix::*;
use crate::pipeline::findings_rounds::*;
use crate::pipeline::patch_review::agent_review_turn;
use crate::pipeline::patch_review_edit::applies_to_pristine;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

#[cfg(test)]
mod tests;

/// What the stage calls out to: the turn rounds.
pub(super) struct FindingsSeams<'a> {
    pub(super) turns: TurnSeams<'a>,
}

/// Step: the findings of a request, in place of the review turn.
pub(super) fn findings_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    job: &FindingsJob,
) -> Result<(), PipelineResult> {
    findings_stage_with(
        ctx,
        st,
        job,
        FindingsSeams {
            turns: TurnSeams {
                turn: &mut agent_findings_turn,
                applies: &mut applies_to_pristine,
                review: &mut agent_review_turn,
            },
        },
    )
}

/// [`findings_stage`] with its seams given. The agent turns record the patches
/// as they were (see `begin_turn`); the record is dropped here, once the stage
/// has settled, however it ended.
pub(super) fn findings_stage_with(
    ctx: &RunCtx,
    st: &mut RunState,
    job: &FindingsJob,
    seams: FindingsSeams<'_>,
) -> Result<(), PipelineResult> {
    let result = stage_inner(ctx, st, job, seams);
    if !take_unsettled() {
        end_turn(&ctx.ws);
    }
    result
}

/// What the stage knows while it runs.
pub(super) struct Findings<'a> {
    pub(super) ctx: &'a RunCtx<'a>,
    pub(super) job: &'a FindingsJob,
    pub(super) step: StepGuard<'a>,
    /// The checkout as it was before the stage.
    pub(super) snapshot: CheckoutSnapshot,
    pub(super) spec_before: String,
}

impl Findings<'_> {
    /// The end record: `fix-end.json` and the notes.
    pub(super) fn end(&self, verdict: &str, reason: &str) {
        let end = FixEnd {
            nonce: self.job.nonce.clone(),
            verdict: verdict.to_string(),
            patch: None,
            applied: None,
            reason: reason.to_string(),
        };
        if let Err(e) = write_fix_end(&self.ctx.ws.dir(), &end) {
            ap_core::diag!("  {e}");
        }
        append_notes(
            self.ctx.ws.notes(),
            &format!(
                "\n## Findings of request {}: {verdict}\n{reason}\n",
                self.job.request_id
            ),
        );
    }

    /// Put the checkout back as it was before the stage and end the package.
    pub(super) fn stop(&self, verdict: &str, kind: FailureKind, reason: String) -> PipelineResult {
        self.snapshot.restore(self.ctx.reporter, Some(&self.step));
        restore_spec(
            &self.ctx.spec_path,
            &self.spec_before,
            Some(&self.step),
            self.ctx.reporter,
        );
        self.end(verdict, &reason);
        self.ctx.failed_as(kind, reason)
    }

    fn report(&self, f: &ReviewFinding, route: &FindingRoute, outcome: &str, reason: &str) {
        let route = match route {
            FindingRoute::Tool(_) => "tool",
            FindingRoute::Agent => "agent",
            FindingRoute::Pipeline => "pipeline",
            FindingRoute::Wait => "wait",
            FindingRoute::Maintainer(_) => "maintainer",
            FindingRoute::NotActedOn => "not_acted_on",
        };
        self.ctx
            .reporter
            .report(&Event::FindingFix(ap_core::fix_event::FindingFixEvent {
                step: Some(self.step.id),
                pkg: self.job.pkg.clone(),
                request: self.job.request_id.clone(),
                finding: f.id.clone(),
                route: route.to_string(),
                outcome: outcome.to_string(),
                reason: reason.to_string(),
            }));
    }
}

/// `edit` applied to the text of the newest `.changes` file of the checkout.
fn edit_changes(
    checkout: &Path,
    edit: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let path = osc::newest_changes_file(checkout).ok_or("the package has no .changes file")?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let fixed = edit(&text)?;
    ap_core::fsx::write_atomic(&path, fixed)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// One tool fix on the checkout; the line says what it did.
fn run_tool(
    fix: MechanicalFix,
    f: &ReviewFinding,
    job: &FindingsJob,
    checkout: &Path,
) -> Result<String, String> {
    let base = job.target_changes.as_deref();
    let spec =
        || std::fs::read_to_string(checkout.join(format!("{}.spec", job.pkg))).unwrap_or_default();
    match fix {
        MechanicalFix::RefreshBullets => {
            edit_changes(checkout, |t| group_refresh_bullets(t, base))?;
            Ok("grouped the Refresh bullets".into())
        }
        MechanicalFix::PatchBullet => {
            let name = f.message.split(" was ").next().unwrap_or_default();
            if !f.message.contains(" was added ") || name.contains(['/', ' ']) || name.is_empty() {
                return Err("only the bullet of an added patch is written without a model".into());
            }
            let patch = std::fs::read_to_string(checkout.join(name))
                .map_err(|e| format!("cannot read {name}: {e}"))?;
            edit_changes(checkout, |t| add_patch_bullet(t, base, name, &patch))?;
            Ok(format!("added the bullet of {name}"))
        }
        MechanicalFix::RestoreOldEntries => {
            edit_changes(checkout, |t| restore_old_entries(t, base))?;
            Ok("put the old entries back".into())
        }
        MechanicalFix::UpdateBullet => {
            edit_changes(checkout, |t| add_update_bullet(t, base, &job.version))?;
            Ok(format!("added `Update to {}`", job.version))
        }
        MechanicalFix::HeaderFormat => {
            edit_changes(checkout, |t| normalise_headers(t, base))?;
            Ok("wrote the entry header in the standard form".into())
        }
        MechanicalFix::DropDependencyBullet => {
            edit_changes(checkout, |t| drop_dependency_bullets(t, base))?;
            Ok("dropped the dependency bullet".into())
        }
        // `lint` fills the notes in: it knows the origin's version, a fix does not.
        MechanicalFix::UpstreamNotes => Err("the notes are filled in by lint".into()),
        MechanicalFix::RemoveLeftover => {
            let name = f.file.as_deref().ok_or("the finding names no file")?;
            let had_series = job.target_files.iter().any(|n| n == "series");
            removable_leftover(name, &spec(), had_series)?;
            std::fs::remove_file(checkout.join(name))
                .map_err(|e| format!("cannot remove {name}: {e}"))?;
            Ok(format!("removed {name}"))
        }
    }
}

fn stage_inner(
    ctx: &RunCtx,
    st: &mut RunState,
    job: &FindingsJob,
    mut seams: FindingsSeams<'_>,
) -> Result<(), PipelineResult> {
    let RunCtx {
        pkg_run,
        reporter,
        rpm_name,
        spec_path,
        checkout_dir,
        ..
    } = ctx;
    // A run that died after the stage settled does not pay for it again.
    if read_fix_end(&ctx.ws.dir(), &job.nonce).is_some_and(|e| e.verdict == "fixed") {
        return Ok(());
    }
    let step = begin_or_fail!(
        pkg_run,
        *reporter,
        StepKind::ValidateSpec,
        Some("fix-findings"),
        &mut st.step_attempts,
        rpm_name.clone()
    );
    let fs = Findings {
        ctx,
        job,
        step,
        snapshot: CheckoutSnapshot::take(checkout_dir),
        spec_before: std::fs::read_to_string(spec_path).unwrap_or_default(),
    };
    // What became of each finding: the outcome and why.
    let mut outcomes: Vec<Option<(&'static str, String)>> = vec![None; job.findings.len()];
    let mut agent: Vec<usize> = Vec::new();
    let mut edited = false;
    // A fix that is not about one file is made once for all its findings.
    let mut once: HashMap<MechanicalFix, Result<String, String>> = HashMap::new();
    for (i, (f, route)) in job.findings.iter().enumerate() {
        outcomes[i] = match route {
            FindingRoute::Tool(fix) => {
                let per_finding = matches!(
                    fix,
                    MechanicalFix::PatchBullet | MechanicalFix::RemoveLeftover
                );
                let done = if per_finding {
                    run_tool(*fix, f, job, checkout_dir)
                } else {
                    once.entry(*fix)
                        .or_insert_with(|| run_tool(*fix, f, job, checkout_dir))
                        .clone()
                };
                match done {
                    Ok(said) => {
                        edited = true;
                        Some(("fixed", said))
                    }
                    Err(why) => match fix.fallback() {
                        FindingRoute::Agent => {
                            agent.push(i);
                            None
                        }
                        FindingRoute::Maintainer(_) => Some(("maintainer", why)),
                        _ => Some(("not_fixed", why)),
                    },
                }
            }
            FindingRoute::Agent => {
                agent.push(i);
                None
            }
            FindingRoute::Pipeline => Some((
                "not_fixed",
                "the build rounds of this run decide".to_string(),
            )),
            FindingRoute::Wait => Some(("not_fixed", "OBS has to finish".to_string())),
            FindingRoute::Maintainer(why) => Some(("maintainer", (*why).to_string())),
            FindingRoute::NotActedOn => Some(("not_acted_on", String::new())),
        };
    }
    if !agent.is_empty() && ctx.config.no_agent {
        for &i in &agent {
            outcomes[i] = Some((
                "not_fixed",
                "needs the agent (--no-agent is set)".to_string(),
            ));
        }
    } else if !agent.is_empty() {
        let asked: Vec<&ReviewFinding> = agent.iter().map(|&i| &job.findings[i].0).collect();
        let end = findings_rounds(&fs, st, &mut seams.turns, &asked)?;
        edited |= end.edited;
        for &i in &agent {
            let answer = end.answers.iter().find(|a| a.id == job.findings[i].0.id);
            outcomes[i] = Some(match answer {
                Some(a) if a.status == FindingStatus::Fixed && end.edited => {
                    ("fixed", a.reason.clone())
                }
                Some(a) if a.status == FindingStatus::NeedsMaintainer => {
                    ("maintainer", a.reason.clone())
                }
                Some(a) => ("not_fixed", a.reason.clone()),
                None => ("not_fixed", "the turn gave no answer for it".to_string()),
            });
        }
    }
    for (i, (f, route)) in job.findings.iter().enumerate() {
        let (outcome, reason) = outcomes[i].clone().unwrap_or(("not_fixed", String::new()));
        fs.report(f, route, outcome, &reason);
    }
    let build_decides = job
        .findings
        .iter()
        .any(|(_, r)| matches!(r, FindingRoute::Pipeline));
    if !edited && !build_decides {
        let why = "nothing in the findings could be changed by this run".to_string();
        return Err(fs.stop("no_fix", FailureKind::NoFix, why));
    }
    fs.end(
        "fixed",
        &format!(
            "{} finding(s) of request {} handled",
            job.findings.len(),
            job.request_id
        ),
    );
    fs.step.finish(StepOutcome::Ok);
    Ok(())
}
