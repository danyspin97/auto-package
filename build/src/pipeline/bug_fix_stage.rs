//! The bug fix stage of `auto-package fix`: the pre-build stage of a fix run,
//! in place of the review. It stages the upstream patch the job found, adds it
//! to the spec without a model, checks it with quilt, has it reviewed, and
//! writes the `.changes` bullet before the first commit. What the mechanical
//! path cannot do goes to the turn rounds (`bug_fix_rounds`).

use super::*;
use crate::bugfix::*;
use crate::patch_check::{PatchStatus, Refresh, declared_patches};
use crate::pipeline::bug_fix_check::{check_fix, unexpected_edits};
use crate::pipeline::bug_fix_rounds::*;
use crate::pipeline::patch_check::obs_tracked_names;
use crate::pipeline::patch_review::*;
use crate::pipeline::rejection::*;
use crate::pipeline::render::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;
use sha2::{Digest, Sha256};

/// Adds the bullet of the fix as a new `.changes` entry (`osc vc`); the
/// message has no leading `- `.
pub(super) type AddEntry<'a> = dyn Fn(&Path, &str) -> Result<(), String> + 'a;

/// What the stage calls out to: the quilt check, the fix turn, the patch review
/// turn and the `.changes` entry. The real ones run quilt, a model and
/// `osc vc`; a test's answer from the checkout.
pub(super) struct FixSeams<'a> {
    pub(super) check: &'a mut PatchCheckFn<'a>,
    pub(super) turn: &'a mut FixTurn<'a>,
    pub(super) review: &'a mut ReviewTurn<'a>,
    pub(super) add_entry: &'a AddEntry<'a>,
}

/// Step: the bug fix, in place of the review turn.
pub(super) fn bug_fix_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    task: &FixTask,
) -> Result<(), PipelineResult> {
    let add_entry = |dir: &Path, message: &str| {
        osc::add_changes_entry(dir, message, Emit::quiet()).map_err(|e| e.to_string())
    };
    bug_fix_stage_with(
        ctx,
        st,
        task,
        FixSeams {
            check: &mut run_patch_check,
            turn: &mut agent_fix_turn,
            review: &mut agent_review_turn,
            add_entry: &add_entry,
        },
    )
}

/// [`bug_fix_stage`] with its seams given. The agent turns record the patches
/// as they were (see `begin_turn`); the record is dropped here, once the stage
/// has settled, however it ended.
pub(super) fn bug_fix_stage_with(
    ctx: &RunCtx,
    st: &mut RunState,
    task: &FixTask,
    seams: FixSeams<'_>,
) -> Result<(), PipelineResult> {
    let result = fix_inner(ctx, st, task, seams);
    if !take_unsettled() {
        end_turn(&ctx.ws);
    }
    result
}

/// What the stage knows while it runs.
pub(super) struct Fix<'a> {
    pub(super) ctx: &'a RunCtx<'a>,
    pub(super) job: &'a FixJob,
    pub(super) step: StepGuard<'a>,
    /// The checkout as it was before anything was staged.
    pub(super) snapshot: CheckoutSnapshot,
    pub(super) spec_before: String,
    /// The patches the spec declared, and the newest `.changes` text, before the stage.
    pub(super) baseline: Vec<String>,
    pub(super) baseline_changes: String,
}

impl Fix<'_> {
    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn event(&self, stage: ap_core::fix_event::BugFixStage) -> ap_core::fix_event::BugFixEvent {
        ap_core::fix_event::BugFixEvent {
            step: Some(self.step.id),
            pkg: self.job.pkg.clone(),
            stage,
            refs: self.job.refs.clone(),
            read: self.job.read.clone(),
            not_read: self.job.not_read.clone(),
            patch: None,
            patch_sha256: None,
            origin: None,
            verdict: None,
            applied: None,
            reason: String::new(),
        }
    }

    /// The end record: `fix-end.json`, the event and the notes.
    pub(super) fn end(
        &self,
        verdict: &str,
        patch: Option<&str>,
        applied: Option<&str>,
        reason: &str,
    ) {
        let end = FixEnd {
            nonce: self.job.nonce.clone(),
            verdict: verdict.to_string(),
            patch: patch.map(str::to_string),
            applied: applied.map(str::to_string),
            reason: reason.to_string(),
        };
        if let Err(e) = write_fix_end(&self.ctx.ws.dir(), &end) {
            ap_core::diag!("  {e}");
        }
        let mut event = self.event(ap_core::fix_event::BugFixStage::End);
        event.patch = end.patch.clone();
        event.verdict = Some(verdict.to_string());
        event.applied = end.applied.clone();
        event.reason = reason.to_string();
        self.ctx.reporter.report(&Event::BugFix(event));
        append_notes(
            self.ctx.ws.notes(),
            &format!(
                "\n## Bug fix: {verdict}{}\n{reason}\n",
                applied.map(|a| format!(" ({a})")).unwrap_or_default()
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
        self.end(verdict, None, None, &reason);
        self.ctx.failed_as(kind, reason)
    }
}

fn fix_inner(
    ctx: &RunCtx,
    st: &mut RunState,
    task: &FixTask,
    mut seams: FixSeams<'_>,
) -> Result<(), PipelineResult> {
    let Some(job) = task.bug() else {
        unreachable!("review_stage sends only a bug fix here")
    };
    let RunCtx {
        pkg_run,
        reporter,
        rpm_name,
        spec_path,
        checkout_dir,
        ..
    } = ctx;
    let spec_before = std::fs::read_to_string(spec_path).unwrap_or_default();
    // A run that died after the fix was settled does not pay for it again.
    if let Some(end) = read_fix_end(&ctx.ws.dir(), &job.nonce)
        && end.verdict == "fixed"
        && end
            .patch
            .as_deref()
            .is_some_and(|p| crate::backport::spec_applies(&spec_before, p))
    {
        return Ok(());
    }
    let step = begin_or_fail!(
        pkg_run,
        *reporter,
        StepKind::ValidateSpec,
        Some("fix-bug"),
        &mut st.step_attempts,
        rpm_name.clone()
    );
    let baseline: Vec<String> = declared_patches(&spec_before)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let fix = Fix {
        ctx,
        job,
        step,
        snapshot: CheckoutSnapshot::take(checkout_dir),
        spec_before: spec_before.clone(),
        baseline,
        baseline_changes: osc::newest_changes_file(checkout_dir)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default(),
    };
    let mut input = fix.event(ap_core::fix_event::BugFixStage::Input);
    if let [first, ..] = job.patches.as_slice() {
        input.patch = Some(first.file_name.clone());
        input.patch_sha256 = Some(Fix::hex(&first.bytes));
        input.origin = Some(first.origin.clone());
    }
    ctx.reporter.report(&Event::BugFix(input));
    append_notes(ctx.ws.notes(), &input_notes(job));
    // A name the spec declares already: the same fix, added by an earlier run, or
    // another patch of that name, which is not ours to judge.
    if let Some(p) = job
        .patches
        .iter()
        .find(|p| fix.baseline.contains(&p.file_name))
    {
        let have = std::fs::read(ctx.checkout_dir.join(&p.file_name)).ok();
        let same = have.as_deref().is_some_and(|b| {
            let (mut a, mut c) = (
                crate::bugfix::code_lines(&String::from_utf8_lossy(&p.bytes)),
                crate::bugfix::code_lines(&String::from_utf8_lossy(b)),
            );
            a.sort();
            c.sort();
            !a.is_empty() && a == c
        });
        return Err(if same {
            let why = format!(
                "{} is declared by the spec already with this fix's code: an earlier run added it",
                p.file_name
            );
            fix.stop("already_fixed", FailureKind::NoFix, why)
        } else {
            let why = format!(
                "the spec already declares a {} whose code is not this fix's: a maintainer \
                 looks at it",
                p.file_name
            );
            fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why)
        });
    }
    if ctx.config.no_agent && job.patches.is_empty() {
        let why = "the fix has no upstream patch to add, which takes the agent (--no-agent is set)";
        return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why.into()));
    }
    // What the mechanical path could not do goes to the turn.
    let retry = match stage_mechanically(&fix, st, &mut seams)? {
        Mechanical::Done => {
            fix.step.finish(StepOutcome::Ok);
            return Ok(());
        }
        Mechanical::NeedsTurn(retry) => retry,
    };
    if ctx.config.no_agent {
        let why = format!("{}; the agent is needed (--no-agent is set)", retry.why());
        return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
    }
    fix_rounds(fix, st, seams, retry)
}

/// How the mechanical path ended.
enum Mechanical {
    /// The staged patches are in, checked, reviewed and named in the `.changes`.
    Done,
    /// The turn must do it: why, and what quilt rejected.
    NeedsTurn(Retry),
}

/// The staged upstream patches, added without a model.
fn stage_mechanically(
    fix: &Fix<'_>,
    st: &mut RunState,
    seams: &mut FixSeams<'_>,
) -> Result<Mechanical, PipelineResult> {
    let (ctx, job) = (fix.ctx, fix.job);
    if job.patches.is_empty() {
        return Ok(Mechanical::NeedsTurn(Retry::none()));
    }
    if let Err(why) = stage_files(fix) {
        return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
    }
    let mut spec = fix.spec_before.clone();
    for p in &job.patches {
        let comment = crate::backport::spec_comment(p, &job.refs);
        spec = match spec_adding_patch(&spec, &comment, &p.file_name) {
            Ok(s) => s,
            Err(why) => return Ok(Mechanical::NeedsTurn(Retry::spec(why))),
        };
    }
    write_or_warn(&ctx.spec_path, &spec);
    let names: Vec<&str> = job.patches.iter().map(|p| p.file_name.as_str()).collect();
    // What quilt says about each staged patch on this version's sources.
    let report = (seams.check)(ctx, Refresh::No);
    let mut applied = "clean";
    for p in &job.patches {
        let Some(result) = report.patches.iter().find(|r| r.name == p.file_name) else {
            let why = format!("quilt did not check {}", p.file_name);
            return Err(fix.stop("does_not_apply", FailureKind::FixDoesNotApply, why));
        };
        match &result.status {
            PatchStatus::Applies | PatchStatus::AppliesWithOffsetOrFuzz { fuzz: false, .. } => {}
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. } => {
                let offsets = (seams.check)(ctx, Refresh::Fuzz);
                for r in offsets
                    .patches
                    .iter()
                    .filter(|r| names.contains(&r.name.as_str()))
                {
                    if let Some(bytes) = &r.refreshed {
                        write_or_warn(ctx.checkout_dir.join(&r.name), bytes);
                    }
                }
                applied = "refreshed";
            }
            PatchStatus::AlreadyApplied { .. } => {
                let why = format!(
                    "{} can be reverse-applied: quilt says its change is in the {} sources already",
                    p.file_name, ctx.config.version
                );
                return Err(fix.stop("already_fixed", FailureKind::NoFix, why));
            }
            status @ (PatchStatus::Fails { .. } | PatchStatus::NotChecked { .. }) => {
                let (reason, rejects) = match status {
                    PatchStatus::Fails { output, rejects } => (output.clone(), rejects.clone()),
                    other => (
                        crate::pipeline::patch_refresh::status_reason(other),
                        String::new(),
                    ),
                };
                return Ok(Mechanical::NeedsTurn(Retry::quilt(
                    &p.file_name,
                    reason,
                    rejects,
                )));
            }
        }
    }
    // The same acceptance gate as a turn's edit: rpmspec, the patch escape, the
    // patch applying, the patch rules.
    if let Err(why) = accept_or_restore_reasoned(
        &ctx.spec_path,
        &fix.spec_before,
        Some(&fix.snapshot),
        &fix.step,
        ctx.reporter,
        &format!("\n## Bug fix — {} staged\n", names.join(", ")),
        ctx.ws.notes(),
        &[],
        &ctx.config.rpm_name,
    ) {
        // The gate put the checkout back, the staged files with it.
        if let Err(e) = stage_files(fix) {
            return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, e));
        }
        return Ok(Mechanical::NeedsTurn(Retry::spec(why)));
    }
    let done = Done {
        patches: job
            .patches
            .iter()
            .map(|p| {
                Fixed::staged(
                    p,
                    None,
                    format!("the upstream fix {}, staged by the tool", p.origin),
                )
            })
            .collect(),
        applied,
    };
    match settle(fix, st, seams, &done, false) {
        Ok(()) => Ok(Mechanical::Done),
        Err(Settle::Retry(why)) => {
            Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why))
        }
        Err(Settle::Stop(result)) => Err(result),
    }
}

/// Put the staged files into the checkout. A name the package tracks with other
/// bytes is a conflict.
pub(super) fn stage_files(fix: &Fix<'_>) -> Result<(), String> {
    let patches = crate::backport::Job {
        patches: fix.job.patches.clone(),
        refs: fix.job.refs.clone(),
    };
    let tracked = obs_tracked_names(&fix.ctx.checkout_dir);
    crate::backport::backport_patches(&fix.ctx.checkout_dir, &patches, &tracked).map(|_| ())
}

/// A patch that carries the fix, as the end of the stage needs to know it.
pub(super) struct Fixed {
    pub(super) patch: crate::backport::PreparedPatch,
    /// The bytes of the staged patch it came from, for the reviewer.
    pub(super) original: Option<Vec<u8>>,
    /// What the review is told changed it.
    pub(super) cause: String,
}

impl Fixed {
    pub(super) fn staged(
        p: &crate::backport::PreparedPatch,
        original: Option<Vec<u8>>,
        cause: String,
    ) -> Fixed {
        Fixed {
            patch: p.clone(),
            original,
            cause,
        }
    }
}

/// The patches that carry the fix, and how they went in.
pub(super) struct Done {
    pub(super) patches: Vec<Fixed>,
    pub(super) applied: &'static str,
}

/// Why the end of the stage did not settle the fix.
pub(super) enum Settle {
    /// A turn can do better: the reason.
    Retry(String),
    /// The package ends.
    Stop(PipelineResult),
}

/// The end of the stage once the patches are in the spec: the patch review,
/// `check_fix`, the `.changes` bullets and the end record. `expected` names the
/// patches that must be the fix; a turn's patch is found as the new one.
pub(super) fn settle(
    fix: &Fix<'_>,
    st: &mut RunState,
    seams: &mut FixSeams<'_>,
    done: &Done,
    from_turn: bool,
) -> Result<(), Settle> {
    let ctx = fix.ctx;
    // The edit may add the fix patch and nothing else: no other patch, no change
    // to another, no other file. What is not the fix is not reviewed either.
    let expected: Vec<&str> = done
        .patches
        .iter()
        .map(|f| f.patch.file_name.as_str())
        .collect();
    let spec_edited = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let stray = unexpected_edits(
        &spec_edited,
        &ctx.checkout_dir,
        &expected,
        &fix.baseline,
        &fix.snapshot,
    );
    if !stray.is_empty() {
        return Err(Settle::Retry(format!(
            "the edit may only add the fix patch to the spec; it also changed {}",
            stray.join(", ")
        )));
    }
    let mut lines: Vec<String> = Vec::new();
    if !ctx.config.no_agent {
        for f in &done.patches {
            let name = &f.patch.file_name;
            let cause = if done.applied == "refreshed" && !from_turn {
                format!("{} and refreshed for this version", f.cause)
            } else {
                f.cause.clone()
            };
            let end = review_patch_rounds(
                ctx,
                st,
                fix.step.id,
                name,
                f.original.as_deref(),
                &cause,
                &mut |ctx, n| stage_applies(ctx, seams.check, n),
                seams.review,
                &mut lines,
            );
            let stop = |why: String| {
                Settle::Stop(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why))
            };
            match end {
                Ok(PatchReviewEnd::Accepted { rewrote }) => {
                    if rewrote
                        && let Err(why) = mark_reviewer_rewrite(&ctx.ws, &ctx.checkout_dir, name)
                    {
                        return Err(stop(why));
                    }
                }
                Ok(PatchReviewEnd::Rejected(why)) => {
                    let why = format!("the patch review rejected {name}: {why}");
                    return Err(if from_turn {
                        Settle::Retry(why)
                    } else {
                        stop(why)
                    });
                }
                Ok(PatchReviewEnd::GaveUp(why)) => {
                    return Err(stop(format!("the patch review gave up on {name}: {why}")));
                }
                Ok(PatchReviewEnd::Unsettled(why)) => {
                    return Err(stop(format!(
                        "the patch review did not settle {name}: {why}"
                    )));
                }
                Err(result) => {
                    fix.snapshot.restore(ctx.reporter, Some(&fix.step));
                    restore_spec(
                        &ctx.spec_path,
                        &fix.spec_before,
                        Some(&fix.step),
                        ctx.reporter,
                    );
                    fix.end("needs_maintainer", None, None, "the patch review stopped");
                    return Err(Settle::Stop(result));
                }
            }
        }
    }
    let spec_now = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    for f in &done.patches {
        let name = &f.patch.file_name;
        let staged = (!from_turn || fix.job.patches.iter().any(|p| &p.file_name == name))
            .then_some(name.as_str());
        if let Err(why) = check_fix(
            &spec_now,
            &ctx.checkout_dir,
            &fix.job.refs,
            staged,
            &fix.baseline,
        ) {
            return Err(Settle::Retry(why));
        }
    }
    for f in &done.patches {
        let name = &f.patch.file_name;
        let bullet = crate::backport::changes_bullet(&f.patch, &fix.job.refs);
        if let Err(why) = set_fix_bullet(
            &ctx.checkout_dir,
            &fix.baseline_changes,
            name,
            &bullet,
            seams.add_entry,
        ) {
            return Err(Settle::Stop(fix.stop(
                "needs_maintainer",
                FailureKind::NeedsMaintainer,
                format!("cannot write the .changes entry: {why}"),
            )));
        }
    }
    let names: Vec<&str> = done
        .patches
        .iter()
        .map(|f| f.patch.file_name.as_str())
        .collect();
    fix.end(
        "fixed",
        names.first().copied(),
        Some(done.applied),
        &format!("{} carries the fix ({})", names.join(", "), done.applied),
    );
    Ok(())
}

/// The `notes.md` block of what the job gave the stage.
fn input_notes(job: &FixJob) -> String {
    let mut text = format!(
        "\n## Bug fix input\nReferences: {}\n",
        if job.refs.is_empty() {
            "none given".to_string()
        } else {
            job.refs.join(" ")
        }
    );
    for r in &job.read {
        text.push_str(&format!("- read {r}\n"));
    }
    for r in &job.not_read {
        text.push_str(&format!("- not read: {r}\n"));
    }
    for p in &job.patches {
        text.push_str(&format!("- staged {} from {}\n", p.file_name, p.origin));
    }
    text
}
