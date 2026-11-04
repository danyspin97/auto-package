//! The bug fix stage of `auto-package fix`: the pre-build stage of a fix run,
//! in place of the review. It stages the upstream patch the job found, adds it
//! to the spec without a model, checks it with quilt, has it reviewed, and
//! writes the `.changes` bullet before the first commit.

use super::*;
use crate::bugfix::*;
use crate::patch_check::{PatchStatus, Refresh, declared_patches};
use crate::pipeline::bug_fix_check::check_fix;
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

/// What the stage calls out to: the quilt check, the patch review turn and the
/// `.changes` entry. The real ones run quilt, a model and `osc vc`; a test's
/// answer from the checkout.
pub(super) struct FixSeams<'a> {
    pub(super) check: &'a mut PatchCheckFn<'a>,
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
struct Fix<'a> {
    ctx: &'a RunCtx<'a>,
    job: &'a FixJob,
    step: StepGuard<'a>,
    /// The checkout as it was before anything was staged.
    snapshot: CheckoutSnapshot,
    spec_before: String,
}

impl Fix<'_> {
    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn record(&self, event: ap_core::fix_event::BugFixEvent) {
        self.ctx.reporter.report(&Event::BugFix(event));
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
    fn end(&self, verdict: &str, patch: Option<&str>, applied: Option<&str>, reason: &str) {
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
        self.record(event);
        append_notes(
            self.ctx.ws.notes(),
            &format!(
                "\n## Bug fix: {verdict}{}\n{reason}\n",
                applied.map(|a| format!(" ({a})")).unwrap_or_default()
            ),
        );
    }

    /// Put the checkout back as it was before the stage and end the package.
    fn stop(self, verdict: &str, kind: FailureKind, reason: String) -> PipelineResult {
        self.snapshot.restore(self.ctx.reporter, Some(&self.step));
        restore_spec(
            &self.ctx.spec_path,
            &self.spec_before,
            Some(&self.step),
            self.ctx.reporter,
        );
        self.end(verdict, None, None, &reason);
        let result = self.ctx.failed_as(kind, reason);
        self.step.finish(StepOutcome::Failed);
        result
    }
}

fn fix_inner(
    ctx: &RunCtx,
    st: &mut RunState,
    task: &FixTask,
    seams: FixSeams<'_>,
) -> Result<(), PipelineResult> {
    let FixTask::Bug(job) = task;
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
    let fix = Fix {
        ctx,
        job,
        step,
        snapshot: CheckoutSnapshot::take(checkout_dir),
        spec_before: spec_before.clone(),
    };
    let mut input = fix.event(ap_core::fix_event::BugFixStage::Input);
    if let [first, ..] = job.patches.as_slice() {
        input.patch = Some(first.file_name.clone());
        input.patch_sha256 = Some(Fix::hex(&first.bytes));
        input.origin = Some(first.origin.clone());
    }
    fix.record(input);
    append_notes(ctx.ws.notes(), &input_notes(job));
    let baseline: Vec<String> = declared_patches(&spec_before)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    // The fix of an earlier run is in the package already.
    if let Some(p) = job.patches.iter().find(|p| baseline.contains(&p.file_name)) {
        let why = format!(
            "{} is declared by the spec already: an earlier run added this fix",
            p.file_name
        );
        return Err(fix.stop("already_fixed", FailureKind::NoFix, why));
    }
    if job.patches.is_empty() {
        return Err(fix.stop(
            "no_fix",
            FailureKind::NeedsMaintainer,
            "the fix has no upstream patch to add, and the turn that finds or writes one is not \
             available"
                .to_string(),
        ));
    }
    stage_mechanically(fix, st, seams, &baseline)
}

/// The staged upstream patches, added without a model.
fn stage_mechanically(
    fix: Fix<'_>,
    st: &mut RunState,
    seams: FixSeams<'_>,
    baseline: &[String],
) -> Result<(), PipelineResult> {
    let (ctx, job) = (fix.ctx, fix.job);
    let FixSeams {
        check,
        review,
        add_entry,
    } = seams;
    let baseline_changes = osc::newest_changes_file(&ctx.checkout_dir)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let patches = crate::backport::Job {
        patches: job.patches.clone(),
        refs: job.refs.clone(),
    };
    let tracked = obs_tracked_names(&ctx.checkout_dir);
    let staged = match crate::backport::backport_patches(&ctx.checkout_dir, &patches, &tracked) {
        Ok(notes) => notes,
        Err(why) => return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why)),
    };
    let mut spec = fix.spec_before.clone();
    for p in &job.patches {
        let comment = crate::backport::spec_comment(p, &job.refs);
        spec = match spec_adding_patch(&spec, &comment, &p.file_name) {
            Ok(s) => s,
            Err(why) => return Err(fix.stop("does_not_apply", FailureKind::FixDoesNotApply, why)),
        };
    }
    write_or_warn(&ctx.spec_path, &spec);
    let names: Vec<&str> = job.patches.iter().map(|p| p.file_name.as_str()).collect();
    // What quilt says about each staged patch on this version's sources.
    let report = check(ctx, Refresh::No);
    let mut applied = "clean";
    for p in &job.patches {
        let Some(result) = report.patches.iter().find(|r| r.name == p.file_name) else {
            let why = format!("quilt did not check {}", p.file_name);
            return Err(fix.stop("does_not_apply", FailureKind::FixDoesNotApply, why));
        };
        match &result.status {
            PatchStatus::Applies | PatchStatus::AppliesWithOffsetOrFuzz { fuzz: false, .. } => {}
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. } => {
                let offsets = check(ctx, Refresh::Fuzz);
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
                let why = format!(
                    "{} does not apply to the {} sources: {}",
                    p.file_name,
                    ctx.config.version,
                    crate::pipeline::patch_refresh::status_reason(status)
                );
                return Err(fix.stop("does_not_apply", FailureKind::FixDoesNotApply, why));
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
        return Err(fix.stop("does_not_apply", FailureKind::FixDoesNotApply, why));
    }
    let mut lines: Vec<String> = Vec::new();
    if !ctx.config.no_agent {
        for p in &job.patches {
            let cause = format!(
                "a bug fix: the upstream fix {}, staged by the tool{}",
                p.origin,
                if applied == "refreshed" {
                    " and refreshed for this version"
                } else {
                    ""
                }
            );
            let end = review_patch_rounds(
                ctx,
                st,
                fix.step.id,
                &p.file_name,
                None,
                &cause,
                &mut |ctx, n| stage_applies(ctx, check, n),
                review,
                &mut lines,
            );
            match end {
                Ok(PatchReviewEnd::Accepted { rewrote }) => {
                    if rewrote
                        && let Err(why) =
                            mark_reviewer_rewrite(&ctx.ws, &ctx.checkout_dir, &p.file_name)
                    {
                        return Err(fix.stop(
                            "needs_maintainer",
                            FailureKind::NeedsMaintainer,
                            why,
                        ));
                    }
                }
                Ok(PatchReviewEnd::Rejected(why)) => {
                    let why = format!("the patch review rejected {}: {why}", p.file_name);
                    return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
                }
                Ok(PatchReviewEnd::GaveUp(why)) => {
                    let why = format!("the patch review gave up on {}: {why}", p.file_name);
                    return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
                }
                Ok(PatchReviewEnd::Unsettled(why)) => {
                    let why = format!("the patch review did not settle {}: {why}", p.file_name);
                    return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
                }
                Err(result) => {
                    fix.snapshot.restore(ctx.reporter, Some(&fix.step));
                    restore_spec(
                        &ctx.spec_path,
                        &fix.spec_before,
                        Some(&fix.step),
                        ctx.reporter,
                    );
                    fix.end("needs_maintainer", None, None, &format!("{result:?}"));
                    fix.step.finish(StepOutcome::Failed);
                    return Err(result);
                }
            }
        }
    }
    let spec_now = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    for p in &job.patches {
        if let Err(why) = check_fix(
            &spec_now,
            &ctx.checkout_dir,
            &job.refs,
            Some(&p.file_name),
            baseline,
        ) {
            return Err(fix.stop("does_not_apply", FailureKind::FixDoesNotApply, why));
        }
    }
    for p in &job.patches {
        let bullet = crate::backport::changes_bullet(p, &job.refs);
        if let Err(why) = set_fix_bullet(
            &ctx.checkout_dir,
            &baseline_changes,
            &p.file_name,
            &bullet,
            add_entry,
        ) {
            let why = format!("cannot write the .changes entry: {why}");
            return Err(fix.stop("needs_maintainer", FailureKind::NeedsMaintainer, why));
        }
    }
    for n in staged.iter().chain(lines.iter()) {
        ap_core::diag!("  {}: {n}", ctx.rpm_name);
    }
    fix.end(
        "fixed",
        names.first().copied(),
        Some(applied),
        &format!("{} added as the fix ({applied})", names.join(", ")),
    );
    fix.step.finish(StepOutcome::Ok);
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
