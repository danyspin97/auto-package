//! The run of one package: the entry point, the run context and state, the build
//! lock, and the stages in order.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::archive::*;
use crate::pipeline::build_round::*;
use crate::pipeline::checkout::*;
use crate::pipeline::commit::*;
use crate::pipeline::deps_turn::*;
use crate::pipeline::generate_stage::*;
use crate::pipeline::patch_stage::*;
use crate::pipeline::review::*;
use crate::pipeline::sdist_stage::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::validate::*;
use crate::pipeline::zig_stage::*;

pub fn run_package(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> PipelineResult {
    let _in_flight = InFlightGuard::acquire(&config.rpm_name);
    let _build_lock = match take_build_lock(config) {
        Ok(lock) => lock,
        Err(failed) => return failed,
    };
    // Counted once the package may write: a run still waiting for another
    // process's build lock has written nothing, and an interrupt does not wait
    // for it. From here on, an interrupt that finds the run between its write
    // and its restore waits for it.
    let _obs_write = crate::proc::obs_write();
    mark_run_started(config, reporter);
    // The fix-loop and risk flags are not touched here: an earlier run's stay
    // until a commit of this run settles them, so a run that ends AlreadyBuilt
    // or missing_deps still reports what OBS holds.
    let baseline = match restore_stale_baseline(config, reporter) {
        Ok(dir) => dir,
        Err(failed) => return failed,
    };
    let result = run_package_inner(config, pkg_run, reporter);
    finish_package(config, pkg_run, reporter, result, &baseline)
}

/// Takes the package's build lock, held for the whole run. This keeps a second
/// `ap` on the same state root from building the same package at once. A
/// failed open is left for `run_package_inner` to report, so it takes no lock.
fn take_build_lock(config: &PipelineConfig) -> Result<Option<std::fs::File>, PipelineResult> {
    let Ok(ws) = Workspace::open(&config.state_root, &config.rpm_name) else {
        return Ok(None);
    };
    let lock_path = ws.dir().join("build.lock");
    match ap_core::proc_lock::lock_with_wait(&lock_path, BUILD_LOCK_WAIT) {
        Ok(f) => Ok(Some(f)),
        Err(e) => {
            let kind = match e {
                ap_core::proc_lock::LockError::Held(_) => FailureKind::Busy,
                ap_core::proc_lock::LockError::Io(_) => FailureKind::Other,
            };
            Err(PipelineResult::Failed {
                rpm_name: config.rpm_name.clone(),
                reason: format!(
                    "could not take {}: {}",
                    lock_path.display(),
                    lock_error_text(&e)
                ),
                kind,
            })
        }
    }
}

/// Records that the run has started: the flags of another version are
/// dropped, and an outcome left by an earlier run is marked as running.
fn mark_run_started(config: &PipelineConfig, reporter: &dyn Reporter) {
    // The flags describe one version's change. A run for another version
    // starts without them: the last version's were never submitted with it.
    // A state that records no version (written before the field) keeps its
    // flags and takes this run's version.
    if let Ok(ws) = Workspace::open(&config.state_root, &config.rpm_name) {
        start_flags_for_version(&ws, &config.version, reporter);
    }
    // The package is being worked on again: its recorded outcome is no longer
    // about the run that is under way. A `missing_deps` or `failed` left from
    // an earlier run read as current while the package built, and when the run
    // was stopped it stayed that way (torchcodec "waiting on ffmpeg" while it
    // was past that).
    if let Ok(ws) = Workspace::open(&config.state_root, &config.rpm_name) {
        let prior = PkgState::load(&ws);
        if matches!(
            prior.last_outcome.as_deref(),
            Some("missing_deps" | "failed" | "interrupted" | WAITING_ON_FAILED)
        ) {
            update_state(&ws, reporter, |state| {
                state.last_outcome = Some("running".to_string());
                state.last_failure_reason = None;
            });
        }
    }
}

/// Puts back a baseline that a failed attempt could not restore, before this
/// run writes anything. Returns the baseline directory; if the restore fails
/// the package does not start.
fn restore_stale_baseline(
    config: &PipelineConfig,
    reporter: &dyn Reporter,
) -> Result<PathBuf, PipelineResult> {
    let baseline = match obs_baseline_dir(config) {
        Ok(dir) => dir,
        Err(e) => {
            return Err(PipelineResult::Failed {
                rpm_name: config.rpm_name.clone(),
                reason: format!("cannot open the package workspace: {e}"),
                kind: FailureKind::Other,
            });
        }
    };
    if baseline.exists()
        && let Err(why) =
            osc::restore_baseline(&baseline, &expected_checkout(config), Emit::run(reporter))
    {
        return Err(PipelineResult::Failed {
            rpm_name: config.rpm_name.clone(),
            reason: format!(
                "not started: the OBS baseline of an earlier failed attempt was NOT \
                 restored ({why}); OBS keeps that attempt. The baseline is kept in {} \
                 and is not deleted; fix the cause and run again, or, if OBS is right \
                 as it is, discard the baseline with `auto-package baseline discard {} \
                 --project {} --arch {} --work-dir {}`",
                baseline.display(),
                config.rpm_name,
                config.project,
                config.arch,
                config
                    .work_dir
                    .parent()
                    .unwrap_or(&config.work_dir)
                    .display(),
            ),
            kind: FailureKind::Other,
        });
    }
    Ok(baseline)
}

/// Settles a finished run: marks what it built, restores the OBS baseline,
/// records why it ended, and runs the notes and release steps for a build.
fn finish_package(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
    result: PipelineResult,
    baseline: &Path,
) -> PipelineResult {
    // What the pipeline itself found built is settled for the rest of the run.
    if matches!(
        result,
        PipelineResult::Built { .. } | PipelineResult::AlreadyBuilt { .. }
    ) {
        mark_settled(&config.rpm_name);
    }
    // Once the run is aborted, whatever else was in flight died of the abort
    // (killed children, refused spawns): report it as the abort, not as the
    // package's own failure.
    let result = match (result, crate::proc::abort_reason()) {
        (
            PipelineResult::Failed {
                rpm_name,
                kind:
                    FailureKind::Other
                    | FailureKind::AgentUnavailable
                    | FailureKind::AgentFailed
                    | FailureKind::Transient
                    | FailureKind::DiskFull
                    | FailureKind::ConditionUnmet,
                ..
            },
            Some(reason),
        ) => PipelineResult::Failed {
            rpm_name,
            reason,
            kind: FailureKind::AgentUnavailable,
        },
        (result, _) => result,
    };
    let expected = expected_checkout(config);
    let result = restore_obs_baseline(baseline, reporter, result, |dir| {
        osc::restore_baseline(dir, &expected, Emit::run(reporter))
    });
    // Persist last-attempt status in the durable workspace so a resumed run
    // (and the TUI showing per-package state across runs) can see WHY the
    // last attempt ended, instead of only "last_outcome: null" on anything
    // that didn't successfully build. Success paths already set
    // `last_outcome` to "success"/"wheel-only" inline — this covers every
    // unsuccessful path, which otherwise left state.json untouched and made
    // a mid-run crash indistinguishable from "never attempted".
    //
    // `MissingDeps` is covered here too, not just `Failed`, so an older
    // `Failed` reason never stays in state.json (see
    // `PkgState::last_failure_reason`).
    let outcome = recorded_outcome(&result);
    let ws = Workspace::open(&config.state_root, &config.rpm_name)
        .inspect_err(|e| {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("opening the package workspace failed: {e}"),
            });
        })
        .ok();
    // `--no-agent` promises no model calls, so the notes turn is skipped too.
    if !config.no_agent
        && !crate::proc::shutting_down()
        && let (PipelineResult::Built { .. }, Some(ws)) = (&result, &ws)
    {
        package_notes_step(config, pkg_run, ws, reporter);
    }
    if let (PipelineResult::Built { .. }, Some(ws)) = (&result, &ws) {
        ws.release_wheel_tree(reporter);
    }
    if let Some((outcome, reason)) = outcome
        && let Some(ws) = &ws
    {
        update_state(ws, reporter, |state| {
            state.last_built_version = Some(config.version.clone());
            state.last_outcome = Some(outcome.to_string());
            state.last_failure_reason = Some(reason);
        });
    }
    result
}

/// What `state` has spent under `run_id`. Spend from an earlier run does not count.
pub(super) fn spent_in_run(state: &PkgState, run_id: &str) -> f64 {
    if state.cumulative_spent_run_id.as_deref() == Some(run_id) {
        state.cumulative_spent_usd
    } else {
        0.0
    }
}

/// Refresh the package's notes as a visible step. It runs after the build has
/// succeeded but before `PackageFinished`, so the TUI must show it: otherwise
/// the package reads as "running" with no step for as long as the model takes.
/// A failure only warns; the package stays built.
fn package_notes_step(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    ws: &Workspace,
    reporter: &dyn Reporter,
) {
    // The notes turn is a model call and counts against the run's budget like
    // any other, so it is refused when its cap would not fit under the budget.
    // An unreadable state file would show no spend: the notes turn is skipped
    // rather than run against a budget it cannot see.
    let prior = match PkgState::try_load(ws) {
        Ok(prior) => prior,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("package notes not updated: cannot read the package state: {e}"),
            });
            return;
        }
    };
    let spent_this_run = spent_in_run(&prior, &config.run_id);
    if spent_this_run + crate::package_notes::NOTES_CAP_USD > config.budget_usd {
        reporter.report(&Event::Warning {
            step: None,
            message: format!(
                "package notes not updated: ${spent_this_run:.2} already spent and the \
                 notes turn can cost up to ${:.2}, over the ${:.2} budget",
                crate::package_notes::NOTES_CAP_USD,
                config.budget_usd
            ),
        });
        return;
    }
    let step = match StepGuard::begin(
        pkg_run,
        reporter,
        StepKind::PackageNotes,
        None,
        &mut Attempts::default(),
    ) {
        Ok(step) => step,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("package notes not updated: {e}"),
            });
            return;
        }
    };
    // Reserve the cap before the call, so the spend is counted even if the
    // process dies during it. The reservation is corrected to the real cost once
    // the turn returns.
    add_run_spend(ws, &config.run_id, crate::package_notes::NOTES_CAP_USD);
    let run = crate::package_notes::update_after_build(
        ws,
        &config.rpm_name,
        &config.version,
        &config.project,
        &config.distro,
        &config.arch,
    );
    let correction = run.usage.cost_usd - crate::package_notes::NOTES_CAP_USD;
    if correction != 0.0 {
        add_run_spend(ws, &config.run_id, correction);
    }
    // Reported even when the update fails: a failed notes turn still cost money.
    if run.usage.cost_usd > 0.0 || run.usage.input_tokens + run.usage.output_tokens > 0 {
        reporter.report(&Event::NotesUsage {
            step: step.id,
            usage: run.usage.clone(),
        });
    }
    let outcome = match run.result {
        Ok(()) => StepOutcome::Ok,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!("package notes not updated: {e}"),
            });
            StepOutcome::Skipped
        }
    };
    step.finish(outcome);
}

/// Reject a name that is about to be joined into a filesystem path as a single
/// component: it must be non-empty, must not contain `/` or `..`, and must not
/// start with `.` (which covers `.` and hidden directories).
pub(super) fn validate_path_component(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} is empty"));
    }
    if value.contains('/') || value.contains("..") || value.starts_with('.') {
        return Err(format!(
            "{label} {value:?} is not a safe path component (no '/', '..', or leading '.')"
        ));
    }
    Ok(())
}

/// What every stage of one package run reads: configuration, the run
/// directory, the reporter, the workspace and the paths derived from the name.
pub(super) struct RunCtx<'a> {
    pub(super) config: &'a PipelineConfig,
    pub(super) pkg_run: &'a PkgRunDir,
    pub(super) reporter: &'a dyn Reporter,
    pub(super) ws: Workspace,
    /// The workspace, state and run key: the lowercase name.
    pub(super) rpm_name: String,
    /// The name OBS knows the package by (see [`resolve_obs_name`]). Every
    /// OBS call about this package, and the checkout directory and spec file
    /// derived from it, use this; the state, logs and display use `rpm_name`.
    pub(super) obs_name: String,
    pub(super) checkout_dir: PathBuf,
    pub(super) spec_path: PathBuf,
}

impl RunCtx<'_> {
    pub(super) fn failed(&self, reason: String) -> PipelineResult {
        self.failed_as(FailureKind::Other, reason)
    }

    /// A failure of an `osc` call: transient when the network, OBS or this
    /// machine failed rather than the package.
    pub(super) fn failed_osc(&self, e: &osc::Error) -> PipelineResult {
        if e.is_disk_full() {
            return self.failed_as(FailureKind::DiskFull, e.to_string());
        }
        self.failed_text(e.is_transient(), e.to_string())
    }

    /// A failure with a message: transient when `transient` says so.
    pub(super) fn failed_text(&self, transient: bool, reason: String) -> PipelineResult {
        let kind = if transient {
            FailureKind::Transient
        } else {
            FailureKind::Other
        };
        self.failed_as(kind, reason)
    }

    pub(super) fn failed_as(&self, kind: FailureKind, reason: String) -> PipelineResult {
        PipelineResult::Failed {
            rpm_name: self.rpm_name.clone(),
            reason,
            kind,
        }
    }

    pub(super) fn turn_env(&self) -> TurnEnv<'_> {
        TurnEnv {
            config: self.config,
            ws: &self.ws,
            pkg_run: self.pkg_run,
            reporter: self.reporter,
            obs_name: &self.obs_name,
        }
    }
}

/// The state that stages mutate as the run proceeds.
pub(super) struct RunState {
    pub(super) step_attempts: Attempts,
    pub(super) sdist_top_level: String,
    pub(super) from_repo_archive: bool,
    /// Fix attempts used so far (persisted for outer-loop retries).
    pub(super) attempts: usize,
    pub(super) spent_usd: f64,
    pub(super) session_id: Option<String>,
    /// SHA-256 of the build log the last fix turn was given, to tell a new
    /// build from OBS handing back the one it already had.
    pub(super) last_log_digest: Option<String>,
    /// Consecutive fix rounds that were given a log identical to the one before.
    pub(super) identical_logs: usize,
    /// The shape of the failure the last fix turn was given (see
    /// [`failure_shape`]) and how many builds in a row failed with it.
    pub(super) last_failure_shape: Option<String>,
    pub(super) same_shape_failures: usize,
    /// The fix-after-build rounds this package has been given (see `build_rounds`).
    pub(super) build_rounds: crate::pipeline::build_rounds::BuildRounds,
    /// SHA-256 of the spec an unresolvable build was retried for after a
    /// bound was relaxed: the retry is only worth a build when the spec
    /// then differs from this.
    pub(super) relaxed_from_spec: Option<String>,
    /// The post-build review ran (or was ruled out) for this run.
    pub(super) post_review_done: bool,
    /// Set while the build of an edit made after a green build (the post-build
    /// review or an rpmlint fix turn) is pending: what to put back when that
    /// build does not succeed.
    pub(super) post_review_revert: Option<PostReviewRevert>,
    /// The rpmlint `E:` keys (see `rpmlint_fix::rpmlint_key`) of this package
    /// version's green builds that a fix turn was given, or that the agent
    /// left: a key in here is not asked about again.
    pub(super) rpmlint_shown: std::collections::BTreeSet<String>,
    /// Set when this run updates a package that already exists (see
    /// `update_mode`): the checks against the origin apply to it.
    pub(super) update: Option<UpdateMode>,
}

/// The state an edit made after a green build is rolled back to.
pub(super) struct PostReviewRevert {
    pub(super) spec: String,
    pub(super) snapshot: CheckoutSnapshot,
    pub(super) kind: RevertKind,
}

/// What the edit a [`PostReviewRevert`] undoes was made for.
pub(super) enum RevertKind {
    PostReview,
    /// The rpmlint `E:` lines of the green build the fix turn was given.
    Rpmlint(Vec<String>),
}

/// What `generate_spec_stage` learned about the package.
pub(super) struct SpecInfo {
    pub(super) need_spec: bool,
    pub(super) is_maturin: bool,
    pub(super) is_rust: bool,
    /// The version whose recipe this spec carries forward, when a version bump
    /// kept the existing spec instead of regenerating it.
    pub(super) carried_from: Option<String>,
}

/// Whether a round of the validate/build loop moves on or goes around again.
pub(super) enum Flow {
    Proceed,
    Retry,
}

/// How a build round ended: another round is needed, or the package is done.
pub(super) enum RoundEnd {
    Retry,
    Finished(PipelineResult),
}

pub(super) fn run_package_inner(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> PipelineResult {
    match run_stages(config, pkg_run, reporter) {
        Ok(result) | Err(result) => result,
    }
}

fn run_stages(
    config: &PipelineConfig,
    pkg_run: &PkgRunDir,
    reporter: &dyn Reporter,
) -> Result<PipelineResult, PipelineResult> {
    let ctx = preflight(config, pkg_run, reporter)?;
    if let Err(reason) = crate::workspace::await_disk_space(&config.work_dir, reporter, None) {
        // A full disk is the machine's, and ends when space is freed.
        return Err(ctx.failed_text(true, reason));
    }
    let mut st = RunState {
        step_attempts: Attempts::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
    };

    let checkout = checkout_stage(&ctx, &mut st)?;
    // A package that builds only on other architectures ends `excluded` on OBS
    // after a whole update and review turn (lsvpd: ppc only, on x86_64).
    if let Ok(text) = std::fs::read_to_string(&ctx.spec_path)
        && let Some(allowed) = crate::spec::exclusive_arch_excluding(&text, &config.arch)
    {
        return Err(ctx.failed(format!(
            "not actionable: the spec's ExclusiveArch ({}) does not include {}",
            allowed.join(" "),
            config.arch
        )));
    }
    let spec = generate_spec_stage(&ctx, &mut st)?;
    osc_service_stage(&ctx, &mut st, &spec)?;
    fetch_sdist_stage(&ctx, &mut st, spec.need_spec)?;
    // The zig vendor archive, rebuilt from the new source, before the first build.
    zig_vendor_stage(&ctx, &mut st)?;
    resume_budget(&ctx, &mut st)?;
    // The carried-forward patches against the new sources, before the review
    // turn and the first commit.
    patch_stage(&ctx, &mut st, &spec)?;

    let build_log_nonempty = checkout.has_failed_build_log(
        ctx.ws
            .build_log()
            .metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false),
    );
    review_stage(&ctx, &mut st, &spec, &checkout, build_log_nonempty)?;
    prefix_stage(
        &ctx,
        &mut st,
        &spec,
        &checkout.obs_status,
        build_log_nonempty,
    )?;

    loop {
        st.step_attempts.round += 1;
        if let Flow::Retry = validate_round(&ctx, &mut st)? {
            continue;
        }
        commit_unless_local(&ctx, &mut st)?;
        // A build retried because a bound was to be relaxed, of a spec that is
        // byte-for-byte the one OBS just called unresolvable, can only say the
        // same again, and takes minutes to say it.
        if let Some(before) = st.relaxed_from_spec.take()
            && sha256_file(&ctx.spec_path).ok().as_deref() == Some(before.as_str())
        {
            return Err(ctx.failed(
                "OBS reported the build dependencies unresolvable on a bound the tool meant to                  relax, but the spec is unchanged after the relaxation: rebuilding it cannot                  change the verdict"
                    .to_string(),
            ));
        }
        match build_round(&ctx, &mut st)? {
            RoundEnd::Retry => continue,
            RoundEnd::Finished(result) => return Ok(result),
        }
    }
}
