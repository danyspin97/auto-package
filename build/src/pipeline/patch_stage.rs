//! The carried-forward patch stage: the patch check and turn, the series and the
//! proven and unproven removals of patches.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::archive::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::patch_loop::*;
use crate::pipeline::patch_review::agent_review_turn;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::removal_proof::*;

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
    // The checkout is reused across runs: a patch a stop left rewritten and
    // unreviewed is never built, whether or not this run carries patches.
    if let Some(stop) = refuse_unreviewed(ctx) {
        return Err(stop);
    }
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
                    carried: false,
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
        &mut agent_review_turn,
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
        .filter(|(_, n)| is_checkout_file(n))
        .map(|(_, n)| {
            let content = std::fs::read(checkout.join(&n)).ok();
            (n, content)
        })
        .collect()
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
        add_patch_bullet(
            ctx,
            step,
            &crate::patch_check::proven_drop_bullet(
                name,
                crate::patch_check::DropProof::ReverseApplies,
            ),
        );
        lines.push(format!(
            "- {name}: removed by the pipeline: already applied upstream (quilt: can be \
             reverse-applied)"
        ));
    }
    lines
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

/// `spec` without the lines of `%prep` that apply the patch numbers in `nums`.
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
    // The forms `applied_patch_number` does not read: `%patch 1` and a shell
    // line naming `%{PATCH1}`.
    crate::patch_check::without_applications(&kept, nums)
}

/// What a kept round did to the patches, made whole and recorded: a proven
/// removal the turn left half done (the `PatchN:` line gone but the file or a
/// `%patch` line left, or the file gone but the line left) is finished, as is a
/// proven disable (the `PatchN:` line kept but no longer applied), a
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
    let (refreshed, mut removed) =
        crate::patch_check::diff_patches(before_files, &after_names, &after_files);
    // A patch the turn kept declared but stopped applying was proven upstream by
    // the gate (it refused every other): it is finished as a removal.
    for (name, _) in crate::patch_check::disabled_patches(before_spec, &spec) {
        if before_files.contains_key(&name) && !removed.contains(&name) {
            removed.push(name);
        }
    }
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
        let Some(proof) = drop_proof(
            &ctx.checkout_dir,
            &ctx.config.version,
            before_spec,
            before_files,
            name,
        ) else {
            // The gate proved every removal it let through, so this is not
            // reached; if it is, no proof is stated.
            lines.push(format!(
                "- {name}: removed by the agent (round {round}): no proof was made, no \
                 .changes bullet"
            ));
            continue;
        };
        add_patch_bullet(
            ctx,
            step,
            &crate::patch_check::proven_drop_bullet(name, proof),
        );
        lines.push(format!(
            "- {name}: removed by the agent (round {round}): proven"
        ));
    }
    !refreshed.is_empty() || !removed.is_empty()
}

/// The run's `.changes` refresh bullet: every patch still in the spec that this
/// release's entry already names, or that this run refreshed, in one bullet
/// (see [`refresh_bullet`](crate::patch_check::refresh_bullet)). It replaces the
/// bullet an earlier run wrote, so the entry keeps one.
pub(super) fn write_refresh_bullets(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    refreshed: &std::collections::BTreeSet<String>,
) {
    let declared = crate::patch_check::declared_patches(
        &std::fs::read_to_string(&ctx.spec_path).unwrap_or_default(),
    );
    let is_declared = |n: &str| declared.iter().any(|(_, d)| d == n);
    let mut names: Vec<String> = osc::listed_refreshes(&ctx.checkout_dir, &ctx.config.version)
        .into_iter()
        .filter(|n| is_declared(n))
        .collect();
    for name in refreshed {
        if is_declared(name) && !names.contains(name) {
            names.push(name.clone());
        }
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    warn_if_err(
        ctx.reporter,
        "adding the .changes entry for the refreshed patches",
        osc::set_refresh_bullet(
            &ctx.checkout_dir,
            &ctx.config.version,
            &crate::patch_check::refresh_bullet(&names),
            Emit::new(ctx.reporter, step),
        ),
    );
}

/// The failure of a package whose patches still do not apply, or that the agent
/// blocked: they are kept, and a human decides. Lists each patch with quilt's
/// output and the rejected hunks, and each blocked patch with the agent's reason.
pub(super) fn kept_patches_reason(
    version: &str,
    stop: &str,
    failing: &[&crate::patch_check::PatchResult],
    blocked: &[crate::agent::PatchAnswer],
) -> String {
    let blocked_only: Vec<&crate::agent::PatchAnswer> = blocked
        .iter()
        .filter(|b| !failing.iter().any(|p| p.name == b.name))
        .collect();
    let mut text = format!(
        "needs a human: {} patch(es) do not apply to the {version} sources or were blocked by \
         the agent, and were kept (a patch is removed only when quilt proves it already applied \
         upstream; {stop}):",
        failing.len() + blocked_only.len()
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
        if let Some(b) = blocked.iter().find(|b| b.name == p.name) {
            text.push_str(&format!("\n  blocked by the agent: {}", b.reason));
        }
    }
    for b in blocked_only {
        text.push_str(&format!(
            "\n- {} (blocked by the agent): {}",
            b.name, b.reason
        ));
    }
    text
}
