//! The prefix stage and the missing dependencies a turn reports.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::apply_turn::*;
use crate::pipeline::dep_edit::*;
use crate::pipeline::ladder::*;
use crate::pipeline::linked::*;
use crate::pipeline::logs::*;
use crate::pipeline::patch_condition::*;
use crate::pipeline::snapshot::*;
use crate::pipeline::spec_edits::*;

/// Step: PreFix (restart with previous build log).
/// When this is a restart (spec was not regenerated) and a previous non-empty
/// build log exists, run an agent turn to fix the spec before a new OBS build.
/// Also runs when OBS already reports the package Unresolvable — that failure
/// mode often never produces a real build log at all, but the agent can
/// still inspect the spec and report what's missing via `unresolvable_packages`.
pub(super) fn prefix_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    spec: &SpecInfo,
    obs_status: &Option<osc::BuildStatus>,
    build_log_nonempty: bool,
) -> Result<(), PipelineResult> {
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
    let log_text = std::fs::read_to_string(ws.build_log()).unwrap_or_default();
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if build_log_nonempty
        && !spec.need_spec
        && let Some(why) = stale_build_log_reason(
            mtime(spec_path),
            mtime(&ws.build_log()),
            &log_text,
            PkgState::load(ws).prefix_log_hash,
        )
    {
        reporter.report(&Event::Progress {
            step: None,
            message: format!("{rpm_name}: no PreFix turn, building again: {why}"),
        });
        return Ok(());
    }
    // Gated like every other fix turn: this path also bumps the attempt count,
    // so without the limit a package could run past --max-attempts (13/10).
    if !spec.need_spec
        && !config.no_agent
        && st.attempts < config.max_attempts
        && st.spent_usd < config.budget_usd
        && (build_log_nonempty || matches!(obs_status, Some(osc::BuildStatus::Unresolvable)))
    {
        let attempts = st.attempts;
        let clean = strip_and_rewrite(spec_path);
        let prefix_log = ws.build_log();
        let extract = extract_build_log(&prefix_log, &st.sdist_top_level, reporter, None);
        let checkout_snapshot = CheckoutSnapshot::take(checkout_dir);

        let turn = agent_turn(
            TurnCtx {
                label: "prefix",
                task: crate::agent::Task::FixAfterBuild,
                event_task: ap_core::event::Task::FixAfterBuild {
                    attempt: attempts as u32 + 1,
                    max_attempts: config.max_attempts as u32,
                },
                attempt: attempts + 1,
                hint: Some(&extract),
                unresolvable_deps: &[],
                unresolvable_detail: &[],
                checkout_dir: Some(checkout_dir),
                from_repo_archive: st.from_repo_archive,
                resume: false,
                context: if !build_log_nonempty {
                    // Same situation the Review turn's context covers above:
                    // an empty build log here is not "nothing to fix", it's
                    // OBS rejecting the build during dependency resolution
                    // before compilation starts. Without this, the pyarrow
                    // case showed the turn reading the empty log, seeing
                    // nothing wrong, and returning `NoChange` instead of
                    // checking BuildRequires names/casing and reporting the
                    // real gap via `unresolvable_packages`.
                    Some(
                        "The build log is empty — OBS rejected the build during \
                         dependency resolution before any compilation started. \
                         The spec may reference a package that does not exist in \
                         OBS yet, or a BuildRequires name may be wrong (wrong \
                         case, or a devel-package name upstream doesn't use — \
                         see convention 2 and convention 27). Do not return \
                         `no_change` without checking every BuildRequires name \
                         against its actual OBS spelling first.",
                    )
                } else {
                    None
                },
            },
            ctx.turn_env(),
            &mut st.spent_usd,
            &mut st.session_id,
            &mut st.step_attempts,
        );

        // The log's hash is recorded only once the turn is applied. A turn that
        // returns MissingDeps (or fails) has not fixed anything, so the same log
        // must get its PreFix again after the deps are built; the stale check
        // would otherwise skip it, since the restored spec is newer than the log.
        if let Ok((prefix_step, outcome)) = turn {
            if let Some(missing) = missing_deps_from_turn(ctx, prefix_step.id, &outcome) {
                // Not applied, so not validated or journaled: undo the turn's
                // edits (see `fix_after_build`).
                checkout_snapshot.restore(reporter, Some(&prefix_step));
                restore_spec(spec_path, &clean, Some(&prefix_step), reporter);
                update_state(ws, reporter, |state| state.prefix_log_hash = None);
                return Err(missing);
            }
            // Everything the turn called missing is already built here, so
            // the turn's edit (and the build that follows it) goes ahead.

            let mut staged: Vec<String> = Vec::new();
            let effect = match apply_turn_and_reset(
                ApplyTurnCtx {
                    config,
                    checkout_dir,
                    spec_path,
                    step: &prefix_step,
                    before: &clean,
                    rpm_name,
                    label: "PreFix — restart from previous log",
                    attempt: attempts + 1,
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
                Ok(effect) => {
                    if build_log_nonempty {
                        let h = hash_spec(&log_text);
                        update_state(ws, reporter, |state| state.prefix_log_hash = Some(h));
                    }
                    effect
                }
                Err(reason) => {
                    update_state(ws, reporter, |state| state.prefix_log_hash = None);
                    resolve_blocked(
                        ctx,
                        st,
                        LadderSite {
                            build_log: Some(&prefix_log),
                            staged: &staged,
                            task: crate::agent::Task::FixAfterBuild,
                            event_task: ap_core::event::Task::FixAfterBuild {
                                attempt: attempts as u32 + 1,
                                max_attempts: config.max_attempts as u32,
                            },
                            attempt: attempts + 1,
                            hint: Some(&extract),
                            context: None,
                            apply_label: "PreFix — restart from previous log",
                            before: &clean,
                            snapshot: &checkout_snapshot,
                            step: &prefix_step,
                        },
                        &outcome,
                        reason,
                    )?
                }
            };
            if matches!(effect, TurnEffect::Edited | TurnEffect::SwitchedToWheel) {
                st.attempts += 1;
                persist_attempts(ws, &config.run_id, st.attempts);
            }
            if effect == TurnEffect::Edited
                && let Some(missing) = missing_deps_added_by_edit(ctx, prefix_step.id, &clean)
            {
                return Err(missing);
            }
        } else if let Err((e, kind)) = turn {
            discard_failed_turn(
                ws.notes(),
                reporter,
                &checkout_snapshot,
                spec_path,
                &clean,
                "PreFix",
            );
            update_state(ws, reporter, |state| state.prefix_log_hash = None);
            return Err(ctx.failed_as(kind, format!("the agent turn failed: {e}")));
        }
    }
    Ok(())
}

/// The `MissingDeps` result for what a fix turn reported missing, or `None`
/// when nothing it named still needs building (or patching) here.
///
/// A `patch_dependencies` entry is a package the distro already ships, so it
/// is only "still needed" while the project has no build of it: once the
/// project has branched and built it, the turn's edit goes ahead.
pub(super) fn missing_deps_from_turn(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    outcome: &crate::agent::TurnOutcome,
) -> Option<PipelineResult> {
    let patch_dependencies: Vec<crate::agent::PatchRequest> = outcome.patch_dependencies.clone();
    if outcome.unresolvable_packages.is_empty() && patch_dependencies.is_empty() {
        return None;
    }
    let mut reported = outcome.unresolvable_packages.clone();
    reported.extend(patch_dependencies.iter().map(|p| p.name.clone()));
    ctx.reporter.report(&Event::MissingPackages {
        step,
        names: reported,
        source: ap_core::event::MissingSource::Agent,
    });
    // The turn's names are triaged (python<ver>- prefix stripped, capabilities
    // and interpreter packages set aside), then the ones the project or a
    // linked project already has are dropped: those are not missing.
    let (named, skipped) = crate::depclass::triage_all(&outcome.unresolvable_packages, &|c| {
        osc::available_to_project(
            &ctx.config.project,
            &ctx.config.arch,
            c,
            Emit::new(ctx.reporter, step),
        )
    });
    warn_skipped(ctx, step, &skipped);
    // A package a linked project ships is only "available" when its version
    // meets what this package declares for it. One that misses the bound is not
    // dropped: too old, it is updated; too new and the build shows an ABI
    // break (an `undefined symbol`), it is patched (convention 62).
    let conflicts = linked_bound_conflicts(ctx, step, &named);
    let mut conflict_patches: Vec<crate::agent::PatchRequest> = Vec::new();
    let mut conflict_outdated: Vec<(String, Option<String>)> = Vec::new();
    for c in &conflicts {
        match c.kind {
            crate::distro_bounds::Kind::DistroTooOld => {
                conflict_outdated.push((c.dep.clone(), None))
            }
            crate::distro_bounds::Kind::DistroTooNew => match abi_symptom(ctx) {
                Some(symptom) => conflict_patches.push(crate::agent::PatchRequest {
                    condition: None,
                    name: c.dep.clone(),
                    problem: format!(
                        "{} {} declares `{} {}`, and {} ships {} {} ({}), which is newer than \
                         that allows. The build fails with what looks like the ABI of the \
                         newer release:\n{symptom}\nFind in the upstream repository what the \
                         newer release removed or changed, and restore it with a patch.",
                        ctx.config.pkg_name,
                        ctx.config.version,
                        c.dep,
                        c.specifier,
                        c.linked_project,
                        c.dep,
                        c.shipped,
                        c.linked_pkg
                    ),
                }),
                None => {
                    // No ABI break to patch: the package uses the distro's
                    // release and its bound is dropped from the spec, then
                    // the build shows whether it works.
                    update_state(&ctx.ws, ctx.reporter, |state| {
                        state.relax_dependency(&c.dep, &c.shipped);
                    });
                    ctx.reporter.report(&Event::Warning {
                        step: Some(step),
                        message: format!(
                            "{} {} ({}) is newer than the bound `{}` that {} declares and \
                             the build shows no ABI break: using it and dropping the bound \
                             from the spec (convention 59)",
                            c.dep, c.shipped, c.linked_project, c.specifier, ctx.config.pkg_name
                        ),
                    });
                }
            },
        }
    }
    let scheduled = |n: &str| {
        conflict_outdated
            .iter()
            .map(|(d, _)| d)
            .chain(conflict_patches.iter().map(|p| &p.name))
            .any(|d| osc::fuzzy_key(d) == osc::fuzzy_key(n))
    };
    let named: Vec<String> = named.into_iter().filter(|n| !scheduled(n)).collect();
    let mut to_build = without_available_deps(ctx, step, named);
    // A patched dependency is one the distro ships by definition: only the
    // project's own build of it ends the request.
    let patch_names: Vec<String> = patch_dependencies.iter().map(|p| p.name.clone()).collect();
    let _ = &patch_names;
    // A request is over when that package was already patched for this very
    // problem: a package this project built (the distro's, branched, or one of
    // its own that a dependent found wrong) is not a reason to drop it, since
    // being built is exactly what the defect is about.
    let already_patched = |p: &crate::agent::PatchRequest| {
        // `p.name` is an RPM name (`ffmpeg-8`, `python-APScheduler`) as often as
        // a PyPI one; the workspace is found by its dependency key.
        dependency_workspace(&ctx.config.state_root, &p.name)
            .map(|w| {
                let real = w
                    .dir()
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let state = PkgState::load(&w);
                // The checkout is made under the OBS spelling the state records
                // (`PkgState::obs_name`), which is not always the workspace name.
                let checkout = state.obs_name.clone().unwrap_or_else(|| real.clone());
                let guess = osc::rpm_name(osc::strip_rpm_prefix(&p.name));
                let over = patch_request_over(&state, p, &|c| {
                    condition_holds_now(ctx.config, &real, &checkout, c)
                        || condition_holds_now(ctx.config, &guess, &checkout, c)
                });
                if over && let Some(cond) = &p.condition {
                    let key = condition_key(cond);
                    if !state.satisfied_conditions.contains(&key)
                        && state.condition_failures_of(key) < CONDITION_GIVE_UP
                    {
                        update_state(&w, ctx.reporter, |s| s.record_condition_satisfied(key));
                    }
                }
                over
            })
            .unwrap_or(false)
    };
    let mut patches: Vec<crate::agent::PatchRequest> = patch_dependencies
        .iter()
        .filter(|p| !already_patched(p))
        .map(|p| crate::agent::PatchRequest {
            condition: p.condition.clone(),
            name: osc::strip_rpm_prefix(&p.name).to_string(),
            problem: p.problem.clone(),
        })
        .collect();
    patches.extend(conflict_patches);
    // A dependency whose patch turn found nothing to restore cannot be patched,
    // however the request is worded: its release newest within this package's
    // bound is built in the project in place of the distro's newer one.
    let (declined, patches): (Vec<_>, Vec<_>) = patches
        .into_iter()
        .partition(|p| patch_declined_before(ctx, &p.name, &p.problem));
    let declined_count = declined.len();
    for p in declined {
        let conflict = linked_bound_conflicts(ctx, step, std::slice::from_ref(&p.name))
            .into_iter()
            .find(|c| c.kind == crate::distro_bounds::Kind::DistroTooNew);
        let release = conflict
            .as_ref()
            .and_then(|c| newest_pypi_release_within(&p.name, &c.specifier));
        match (conflict, release) {
            (Some(c), Some(version)) => {
                ctx.reporter.report(&Event::Warning {
                    step: Some(step),
                    message: format!(
                        "{} could not be patched for {} {}: building its {version}, the newest \
                         release within `{}`, in {} instead of using {} {} from {}",
                        p.name,
                        ctx.config.pkg_name,
                        ctx.config.version,
                        c.specifier,
                        ctx.config.project,
                        p.name,
                        c.shipped,
                        c.linked_project
                    ),
                });
                conflict_outdated.push((p.name.clone(), Some(version)));
            }
            // No DistroTooNew conflict bounds this package against the
            // dependency, so there is no bound to build an older release under.
            (None, _) => ctx.reporter.report(&Event::Warning {
                step: Some(step),
                message: format!(
                    "{} could not be patched, and no distro version of it conflicts with {} {}, \
                     so there is no older release to build in its place",
                    p.name, ctx.config.pkg_name, ctx.config.version
                ),
            }),
            // A conflict bounds it, but PyPI has no release within that bound.
            (Some(c), None) => ctx.reporter.report(&Event::Warning {
                step: Some(step),
                message: format!(
                    "{} could not be patched, and PyPI has no release of it within `{}` to \
                     build in its place",
                    p.name, c.specifier
                ),
            }),
        }
    }
    if declined_count > 0
        && to_build.is_empty()
        && conflict_outdated.is_empty()
        && patches.is_empty()
    {
        ctx.reporter.report(&Event::Warning {
            step: Some(step),
            message: format!(
                "the declined dependency requests leave nothing to build or patch for {}",
                ctx.config.pkg_name
            ),
        });
    }
    for (dep, _) in &conflict_outdated {
        if !to_build
            .iter()
            .any(|d| osc::fuzzy_key(d) == osc::fuzzy_key(dep))
        {
            to_build.push(dep.clone());
        }
    }
    for p in &patches {
        if !to_build
            .iter()
            .any(|d| osc::fuzzy_key(d) == osc::fuzzy_key(&p.name))
        {
            to_build.push(p.name.clone());
        }
    }
    if to_build.is_empty() {
        return None;
    }
    let native = crate::depclass::native_among(&outcome.unresolvable_packages, &to_build);
    Some(PipelineResult::MissingDeps {
        rpm_name: ctx.rpm_name.clone(),
        deps: to_build,
        patches,
        native,
        outdated: conflict_outdated,
    })
}
