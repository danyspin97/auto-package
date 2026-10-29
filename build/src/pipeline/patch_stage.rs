//! The carried-forward patch stage: the patch check and turn, the series and the
//! proven and unproven removals of patches.

use super::*;

// ── carried-forward patches, tested with quilt ───────────────────────────────

/// Runs the quilt check of the spec on disk: the real one runs quilt, a test's
/// answers from the checkout.
pub(super) type PatchCheckFn<'a> =
    dyn FnMut(&RunCtx, crate::patch_check::Refresh) -> crate::patch_check::CheckReport + 'a;

/// Runs one RefreshPatches agent turn, given the attempt number and the context.
pub(super) type PatchTurn<'a> = dyn FnMut(
        &RunCtx,
        &mut RunState,
        usize,
        &str,
    ) -> Result<(StepDir, crate::agent::TurnOutcome), (String, FailureKind)>
    + 'a;

/// Step: the patches a version bump carried forward, pushed with quilt onto the
/// new sources before the review turn and the first commit. A patch that does
/// not apply cleanly is the agent's to rebase or remove, for up to
/// [`crate::patch_check::MAX_ROUNDS`] rounds while each makes progress; only a
/// patch still failing after that is dropped by the pipeline.
pub(super) fn patch_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    spec: &SpecInfo,
) -> Result<(), PipelineResult> {
    if spec.carried_from.is_none() || crate::redist::is_redist_package(&ctx.config.rpm_name) {
        return Ok(());
    }
    let text = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    if crate::patch_check::declared_patches(&text).is_empty() {
        return Ok(());
    }
    patch_stage_with(
        ctx,
        st,
        &mut run_patch_check,
        &mut |ctx, st, attempt, context| {
            agent_turn(
                TurnCtx {
                    label: "refresh-patches",
                    task: crate::agent::Task::RefreshPatches,
                    event_task: ap_core::event::Task::FixAfterBuild {
                        attempt: attempt as u32,
                        max_attempts: ctx.config.max_attempts as u32,
                    },
                    attempt,
                    hint: None,
                    unresolvable_deps: &[],
                    unresolvable_detail: &[],
                    checkout_dir: Some(&ctx.checkout_dir),
                    from_repo_archive: st.from_repo_archive,
                    resume: false,
                    context: Some(context),
                },
                ctx.turn_env(),
                &mut st.spent_usd,
                &mut st.session_id,
                &mut st.step_attempts,
            )
        },
    )
}

/// The quilt check of the spec on disk against this version's source archive.
pub(super) fn run_patch_check(
    ctx: &RunCtx,
    refresh: crate::patch_check::Refresh,
) -> crate::patch_check::CheckReport {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let tarball = find_tarball(&ctx.checkout_dir, &ctx.config.version);
    crate::patch_check::check(&spec, &ctx.checkout_dir, tarball.as_deref(), refresh)
}

/// Every patch `spec` declares with its file's content in `checkout` (`None`
/// when the file is not there).
pub(super) fn patch_files(
    spec: &str,
    checkout: &Path,
) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
    crate::patch_check::declared_patches(spec)
        .into_iter()
        .filter(|(_, n)| !n.is_empty() && Path::new(n).file_name() == Some(n.as_ref()))
        .map(|(_, n)| {
            let content = std::fs::read(checkout.join(&n)).ok();
            (n, content)
        })
        .collect()
}

/// A quilt `series` for the turn's quilt (QUILT_PATCHES points it at the
/// checkout), in the order and with the levels rpm applies the patches with,
/// written only when the checkout has none: an existing one is left exactly as
/// it is. The pipeline's own checks use a scratch series (see `patch_check`).
pub(super) fn write_patch_series(spec: &str, checkout: &Path) {
    if checkout.join("series").exists() {
        return;
    }
    let entries: Vec<crate::patch_check::SeriesEntry> = crate::patch_check::plan_series(spec)
        .into_iter()
        .filter_map(|p| match p {
            crate::patch_check::Planned::Check(e) => Some(e),
            crate::patch_check::Planned::Skip { .. } => None,
        })
        .collect();
    if !entries.is_empty() {
        write_or_warn(
            checkout.join("series"),
            crate::patch_check::series_text(&entries),
        );
    }
}

/// Take every quilt patch off `source/`, so each round starts from the
/// pristine new sources (quilt puts the files back from its `.pc` copies).
pub(super) fn pop_source_patches(ctx: &RunCtx) {
    let source = ctx.ws.dir().join("source");
    if !source.join(".pc").exists() {
        return;
    }
    let mut env = crate::proc::minimal_env(&[]);
    env.insert(
        "QUILT_PATCHES".into(),
        ctx.checkout_dir.display().to_string(),
    );
    env.insert("QUILTRC".into(), "/dev/null".into());
    env.insert("LC_ALL".into(), "C".into());
    let popped = crate::proc::capture_with_timeout_env(
        "quilt",
        &["pop", "-a", "-f"],
        &source,
        Emit::quiet(),
        Duration::from_secs(120),
        Some(&env),
    );
    // 2 is "No patch removed": nothing was applied.
    match popped {
        Ok(out) if out.code == 0 || out.code == 2 => {}
        Ok(out) => ap_core::diag!(
            "  quilt pop -a in source/ failed: {}{}",
            out.stdout.trim(),
            out.stderr.trim()
        ),
        Err(e) => ap_core::diag!("  quilt pop -a in source/ could not run: {e}"),
    }
}

/// Add one `.changes` bullet for a patch to this release's entry.
fn add_patch_bullet(ctx: &RunCtx, step: ap_core::event::StepId, bullet: &str) {
    warn_if_err(
        ctx.reporter,
        "adding the .changes entry for a patch",
        osc::add_release_changes_entry(
            &ctx.checkout_dir,
            &ctx.config.version,
            bullet,
            Emit::new(ctx.reporter, step),
        ),
    );
}

/// Remove the patches quilt proved already applied in the new sources (the
/// one proof a removal needs), from the spec and the checkout, with one
/// `.changes` bullet each that states the proof. Returns a notes line each.
pub(super) fn remove_proven_patches(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    names: &[String],
) -> Vec<String> {
    if names.is_empty() {
        return Vec::new();
    }
    let Ok(current) = std::fs::read_to_string(&ctx.spec_path) else {
        return Vec::new();
    };
    write_or_warn(&ctx.spec_path, without_patches(&current, names));
    drop_series_lines(&ctx.checkout_dir, names);
    let mut lines = Vec::new();
    for name in names {
        let _ = std::fs::remove_file(ctx.checkout_dir.join(name));
        ctx.reporter.report(&Event::Warning {
            step: Some(step),
            message: format!(
                "{}: removed {name}: quilt reports it already applied in the {} sources \
                 (`can be reverse-applied`)",
                ctx.rpm_name, ctx.config.version
            ),
        });
        add_patch_bullet(ctx, step, &crate::patch_check::proven_drop_bullet(name));
        lines.push(format!(
            "- {name}: removed by the pipeline: already applied upstream (quilt: can be \
             reverse-applied)"
        ));
    }
    lines
}

/// The patches a turn removed (its `PatchN:` line or its file) whose removal is
/// not proven: quilt, run on the new sources with the patches as they were
/// before the turn, does not report them `can be reverse-applied`, so their
/// change is not upstream. Empty when the turn removed nothing or every
/// removal is proven. `snapshot` holds the patch files as they were.
pub(super) fn unproven_patch_removals(
    config: &PipelineConfig,
    checkout_dir: &Path,
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
) -> Vec<String> {
    let before_files: std::collections::BTreeMap<String, Option<Vec<u8>>> =
        crate::patch_check::declared_patches(before)
            .into_iter()
            .filter(|(_, n)| !n.is_empty() && Path::new(n).file_name() == Some(n.as_ref()))
            .map(|(_, n)| {
                let content = snapshot
                    .and_then(|s| s.content_of(&n))
                    .or_else(|| std::fs::read(checkout_dir.join(&n)).ok());
                (n, content)
            })
            .collect();
    if before_files.is_empty() {
        return Vec::new();
    }
    let after = std::fs::read_to_string(spec_path).unwrap_or_default();
    let after_names: Vec<String> = crate::patch_check::declared_patches(&after)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let after_files = before_files
        .keys()
        .map(|n| (n.clone(), std::fs::read(checkout_dir.join(n)).ok()))
        .collect();
    let (_, removed) = crate::patch_check::diff_patches(&before_files, &after_names, &after_files);
    if removed.is_empty() {
        return Vec::new();
    }
    let tarball = find_tarball(checkout_dir, &config.version);
    let report = crate::patch_check::check_files(before, &before_files, tarball.as_deref());
    removed
        .into_iter()
        .filter(|n| !report.proves_applied(n))
        .collect()
}

/// What a turn is told when its removal of `names` was refused.
pub(super) fn unproven_removal_problem(names: &[String]) -> String {
    names
        .iter()
        .map(|n| {
            format!(
                "removal of {n} is not proven: quilt does not report it `can be \
                 reverse-applied` on the new sources, so its change is not in them; rebase it \
                 with quilt (`quilt push -f`, fix the rejected hunks, `quilt refresh`)"
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Roll back a turn whose edit removed patches without proof, and journal why.
pub(super) fn reject_unproven_removal(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    label: &str,
    unproven: &[String],
) -> String {
    let problem = unproven_removal_problem(unproven);
    restore_spec(spec_path, before, Some(step), reporter);
    if let Some(snap) = snapshot {
        snap.restore_keeping(reporter, Some(step), keep);
    }
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::Warning {
        step: Some(step.id),
        message: format!("{label}: edit rejected: {problem}"),
    });
    append_notes(
        notes_path,
        &format!(
            "\n## {label} — edit rejected: a patch removal is not proven\n{problem}\n\
             A patch is never removed because it does not apply. The edit was rolled back.\n"
        ),
    );
    problem
}

/// How a RefreshPatches turn's outcome was applied.
#[derive(Debug, PartialEq)]
pub(super) enum PatchTurnApplied {
    /// The edit was kept.
    Kept,
    /// Nothing was kept: no edit, or a spec rpmspec refuses.
    RolledBack,
    /// The edit removed patches without proof and was rolled back; what to tell
    /// the next round.
    Refused(String),
}

/// Apply a RefreshPatches turn's outcome. An edit is kept when the spec still
/// parses, no patch escapes the tree and every patch it removed is proven
/// already applied; whether the patches apply is the next quilt check's to say
/// (the usual gate would refuse the edit while any patch still fails, which
/// would throw away a round that fixed one of several). Anything else is
/// rolled back.
pub(super) fn apply_patch_turn(
    ctx: &RunCtx,
    step: &StepDir,
    outcome: &crate::agent::TurnOutcome,
    before: &str,
    snapshot: &CheckoutSnapshot,
    round: usize,
) -> PatchTurnApplied {
    let (reporter, spec_path, notes) = (ctx.reporter, &ctx.spec_path, ctx.ws.notes());
    let label = format!("RefreshPatches — round {round}");
    if !matches!(outcome.action, crate::agent::Action::Edited) {
        snapshot.restore_keeping(reporter, Some(step), &[]);
        restore_spec(spec_path, before, Some(step), reporter);
        append_notes(
            notes,
            &format!(
                "\n## {label} — no change\nReasoning: {}\n",
                brief(&outcome.reasoning)
            ),
        );
        return PatchTurnApplied::RolledBack;
    }
    match osc::rpmspec_check(spec_path, Emit::new(reporter, step.id)) {
        Ok(_) => {}
        Err(e @ (osc::Error::RpmspecFailed { .. } | osc::Error::UnsandboxedMacro { .. })) => {
            restore_spec(spec_path, before, Some(step), reporter);
            snapshot.restore_keeping(reporter, Some(step), &[]);
            append_notes(notes, &rollback_note(&e.to_string()));
            return PatchTurnApplied::RolledBack;
        }
        Err(e) => skipped_check_note(reporter, step, notes.clone(), "rpmspec", &e.to_string()),
    }
    if reject_patch_escape(
        spec_path,
        before,
        Some(snapshot),
        step,
        reporter,
        notes.clone(),
        &[],
    )
    .is_some()
    {
        return PatchTurnApplied::RolledBack;
    }
    let unproven = unproven_patch_removals(
        ctx.config,
        &ctx.checkout_dir,
        spec_path,
        before,
        Some(snapshot),
    );
    if !unproven.is_empty() {
        return PatchTurnApplied::Refused(reject_unproven_removal(
            spec_path,
            before,
            Some(snapshot),
            step,
            reporter,
            notes,
            &[],
            &label,
            &unproven,
        ));
    }
    append_notes(
        notes,
        &format!(
            "\n## {label}\nReasoning: {}\n{}",
            outcome.reasoning,
            evidence_lines(&outcome.evidence)
        ),
    );
    PatchTurnApplied::Kept
}

/// The checkout's `series` without exactly the lines of `names`; every other
/// line keeps its text and place. No `series`, nothing to do.
pub(super) fn drop_series_lines(checkout: &Path, names: &[String]) {
    let path = checkout.join("series");
    let Ok(current) = std::fs::read_to_string(&path) else {
        return;
    };
    let kept: String = current
        .split_inclusive('\n')
        .filter(|line| {
            line.split_whitespace()
                .next()
                .is_none_or(|first| !names.iter().any(|n| n == first))
        })
        .collect();
    if kept != current {
        write_or_warn(&path, kept);
    }
}

/// `spec` without the `%patch` lines that apply the patch numbers in `nums`.
pub(super) fn without_patch_applications(spec: &str, nums: &[String]) -> String {
    let mut kept = String::new();
    for line in spec.lines() {
        if !applied_patch_number(line)
            .is_some_and(|n| nums.iter().any(|m| same_patch_number(m, &n)))
        {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// What a kept round did to the patches, made whole and recorded: a proven
/// removal the turn left half done (the `PatchN:` line gone but the file or a
/// `%patch` line left, or the file gone but the line left) is finished, a
/// removed patch gets its `.changes` bullet, a refreshed one is added to
/// `refreshed` (its bullet is written once the rounds are over, when it is
/// still there), and each gets a line in `lines`. `true` when the round changed
/// or removed a patch.
pub(super) fn settle_patch_changes(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    before_spec: &str,
    before_files: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    round: usize,
    lines: &mut Vec<String>,
    refreshed_all: &mut std::collections::BTreeSet<String>,
) -> bool {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let after_names: Vec<String> = crate::patch_check::declared_patches(&spec)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    let after_files = before_files
        .keys()
        .map(|n| (n.clone(), std::fs::read(ctx.checkout_dir.join(n)).ok()))
        .collect();
    let (refreshed, removed) =
        crate::patch_check::diff_patches(before_files, &after_names, &after_files);
    if !removed.is_empty() {
        let nums: Vec<String> = crate::patch_check::declared_patches(before_spec)
            .into_iter()
            .filter(|(_, n)| removed.contains(n))
            .map(|(num, _)| num)
            .collect();
        let fixed = without_patch_applications(&without_patches(&spec, &removed), &nums);
        if fixed != spec {
            write_or_warn(&ctx.spec_path, &fixed);
        }
        for name in &removed {
            let _ = std::fs::remove_file(ctx.checkout_dir.join(name));
        }
        drop_series_lines(&ctx.checkout_dir, &removed);
    }
    for name in &refreshed {
        refreshed_all.insert(name.clone());
        lines.push(format!("- {name}: refreshed by the agent (round {round})"));
    }
    for name in &removed {
        add_patch_bullet(ctx, step, &crate::patch_check::proven_drop_bullet(name));
        lines.push(format!(
            "- {name}: removed by the agent (round {round}): already applied upstream (quilt: \
             can be reverse-applied)"
        ));
    }
    !refreshed.is_empty() || !removed.is_empty()
}

/// One `.changes` bullet per refreshed patch that is still in the spec.
pub(super) fn write_refresh_bullets(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    refreshed: &std::collections::BTreeSet<String>,
) {
    let declared = crate::patch_check::declared_patches(
        &std::fs::read_to_string(&ctx.spec_path).unwrap_or_default(),
    );
    for name in refreshed {
        if declared.iter().any(|(_, n)| n == name) {
            add_patch_bullet(ctx, step, &crate::patch_check::refresh_bullet(name));
        }
    }
}

/// The failure of a package whose patches still do not apply: they are kept,
/// and a human decides. Lists each patch with quilt's output and the rejected
/// hunks.
pub(super) fn kept_patches_reason(
    version: &str,
    stop: &str,
    failing: &[&crate::patch_check::PatchResult],
) -> String {
    let mut text = format!(
        "needs a human: {} patch(es) do not apply to the {version} sources and were kept (a \
         patch is removed only when quilt proves it already applied upstream; {stop}):",
        failing.len()
    );
    for p in failing {
        if let crate::patch_check::PatchStatus::Fails { output, rejects } = &p.status {
            text.push_str(&format!(
                "\n- {}:\n{}",
                p.name,
                crate::patch_check::truncate(output, 600)
            ));
            if !rejects.is_empty() {
                text.push_str(&format!(
                    "\n  rejected hunks:\n{}",
                    crate::patch_check::truncate(rejects, 1500)
                ));
            }
        } else {
            text.push_str(&format!("\n- {}", p.name));
        }
    }
    text
}
