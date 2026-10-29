//! Applying a turn's outcome to the checkout and resetting it.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::keep_version::*;
use crate::pipeline::licence::*;
use crate::pipeline::npm_vendor::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::python_spec::*;

/// Everything [`apply_turn_outcome`] needs besides the outcome itself and the
/// reporter — bundled so the function doesn't trip clippy's argument-count
/// lint the way its five call sites' worth of parameters would inline.
pub(super) struct ApplyTurnCtx<'a> {
    pub(super) config: &'a PipelineConfig,
    pub(super) checkout_dir: &'a Path,
    pub(super) spec_path: &'a Path,
    pub(super) step: &'a StepDir,
    /// Spec content before this turn ran, for `accept_or_restore`'s diff.
    pub(super) before: &'a str,
    pub(super) rpm_name: &'a str,
    /// Short description of this turn used in both the notes.md heading and
    /// any failure reason (`"FixAfterBuild"`, `"Review — pre-build"`, ...).
    pub(super) label: &'a str,
    pub(super) attempt: usize,
    pub(super) notes_path: PathBuf,
    /// Full-checkout snapshot taken BEFORE the agent turn ran. When `Some`,
    /// a rejected / blocked / no-change outcome rolls back every top-level
    /// checkout file the agent may have edited (patches, `.changes`,
    /// `_service`, …), not just the spec. Each
    /// call site opts in by capturing a snapshot right before `agent_turn`
    /// and passing `Some(&snap)` here. Sites that don't opt in get the
    /// spec-only restore (the default).
    pub(super) snapshot: Option<&'a CheckoutSnapshot>,
    /// Files staged into the checkout by earlier rounds of the same resolution
    /// ladder. Every restore keeps them, so a later round's rollback does not
    /// delete what an earlier round was asked to copy in.
    pub(super) kept: &'a [String],
    /// Where the names of the files this turn actually copied into the checkout
    /// are appended (a refused request is not one of them). The resolution
    /// ladder keeps them for its later rounds; every other caller passes `None`.
    pub(super) staged_out: Option<&'a mut Vec<String>>,
    /// Where the reason of a rejected edit goes (the gate's own words), so the
    /// ladder's history can say why a round changed nothing. `None` when nothing
    /// reads it.
    pub(super) rejected_out: Option<&'a mut Option<String>>,
}

/// Apply one agent turn's outcome the same way at every call site that runs
/// a fix turn (Review, PreFix, FixAfterBuild — both the unresolvable-deps and
/// generic-failure shapes — and FixAfterValidation): accept an edit, switch
/// to the upstream wheel and accept that instead, or report nothing changed.
///
/// Centralizing this is what makes `Action::UseUpstreamWheel` (convention 28
/// Exception 4) actually available at *every* turn instead of only the ones
/// whose call site happened to be updated — that gap is exactly how it went
/// missing at three separate sites the first time each was patched by hand.
/// `Blocked` and a failed wheel fallback are returned as `Err` for the
/// caller to fold into its own `PipelineResult::Failed`.
///
/// Returns [`TurnEffect::SwitchedToWheel`] as a distinct case from
/// `Edited` — not just `Ok(true)` — because switching replaces the entire
/// spec with a wheel skeleton: `sdist_top_level` and `from_repo_archive`,
/// both computed once from the *original* source at the top of
/// `run_package`, describe a source tree that no longer has anything to do
/// with the spec on disk (a wheel unpacks flat, and it isn't a repository
/// archive). The caller resets them; folding a wheel switch into the same
/// variant as an ordinary edit would leave those stale — a wrong
/// `sdist_top_level` mainly degrades traceback-file matching in
/// `extract_build_log`, not silently corrupts the package, but there is no
/// reason to leave it wrong once we know better.
fn apply_turn_outcome_with(
    ctx: ApplyTurnCtx<'_>,
    outcome: &crate::agent::TurnOutcome,
    reporter: &dyn Reporter,
    lookup: PypiLookup<'_>,
) -> Result<TurnEffect, String> {
    let ApplyTurnCtx {
        config,
        checkout_dir,
        spec_path,
        step,
        before,
        rpm_name,
        label,
        attempt,
        notes_path,
        snapshot,
        kept,
        mut staged_out,
        mut rejected_out,
    } = ctx;
    // Files the turn asked the pipeline to copy into the checkout: its shell
    // cannot write there. Done first, and kept even by a `blocked` or
    // `no_change` answer: the copy is the work the turn asked for.
    let staged = stage_requested_files(
        config,
        checkout_dir,
        step,
        outcome,
        reporter,
        &notes_path,
        label,
    );
    if let Some(out) = staged_out.as_mut() {
        out.extend(staged.iter().cloned());
    }
    // Everything an earlier round staged, and this turn's own staged files.
    let keep: Vec<String> = kept.iter().chain(staged.iter()).cloned().collect();
    // A `blocked` or `no_change` answer that staged files: the staged files
    // are kept by every restore below (`keep`), and the outcome is the one the
    // plain arm gives for the same answer. A patch the turn wrote that escapes
    // the tree is rolled back here, with the staged files kept, before that.
    if matches!(
        outcome.action,
        crate::agent::Action::Blocked { .. } | crate::agent::Action::NoChange
    ) && !staged.is_empty()
    {
        append_notes(
            notes_path.clone(),
            &format!(
                "\n## Attempt {attempt} — {label} — staged files\nStaged into the checkout: \
                 {}\nReasoning: {}\n",
                staged.join(", "),
                brief(&outcome.reasoning)
            ),
        );
        let escaped = reject_patch_escape(
            spec_path,
            before,
            snapshot,
            step,
            reporter,
            notes_path.clone(),
            &keep,
        )
        .is_some();
        if escaped && let Some(msg) = repeated_spec_rejection(spec_path) {
            return Err(format!("{label}: {msg}"));
        }
    }
    let effect = match &outcome.action {
        crate::agent::Action::Blocked { why } => {
            // On Blocked, restore the full
            // checkout too — the agent may have written speculative
            // patches/_service files before deciding it's blocked, and
            // those should not survive the rejection.
            if let Some(snap) = snapshot {
                snap.restore_keeping(reporter, Some(step), &keep);
            }
            restore_spec(spec_path, before, Some(step), reporter);
            // Recorded as a fix-loop flag: a blocked answer is a result the
            // changelog, the request and the summary must show.
            if let Ok(ws) = Workspace::open(&config.state_root, rpm_name) {
                update_state(&ws, reporter, |st| {
                    crate::fix_flags::add_blocked(st, &config.run_id, why.clone());
                });
            }
            // Journaled like an edit: a turn that changed nothing is still what
            // the next turn, and whoever reads the notes, needs to know about
            // (python-chromadb's ten identical turns left no trace in them).
            append_notes(
                notes_path,
                &format!(
                    "\n## Attempt {attempt} — {label} — blocked\nReasoning: {}\n",
                    brief(why)
                ),
            );
            Err(format!("{label}: agent blocked: {why}"))
        }
        crate::agent::Action::NoChange => {
            // On NoChange, also restore — the agent's "no change needed"
            // verdict shouldn't paper over an unreported partial edit.
            if let Some(snap) = snapshot {
                snap.restore_keeping(reporter, Some(step), &keep);
            }
            append_notes(
                notes_path,
                &format!(
                    "\n## Attempt {attempt} — {label} — no change\nReasoning: {}\n",
                    brief(&outcome.reasoning)
                ),
            );
            Ok(TurnEffect::NoChange)
        }
        crate::agent::Action::Edited => {
            // A patch is removed only with quilt's proof that its change is
            // already in the new sources; never because it does not apply.
            let unproven =
                unproven_patch_removals(config, checkout_dir, spec_path, before, snapshot);
            let changed = if !unproven.is_empty() {
                let why = reject_unproven_removal(
                    spec_path,
                    before,
                    snapshot,
                    step,
                    reporter,
                    notes_path.clone(),
                    &keep,
                    &format!("Attempt {attempt} — {label}"),
                    &unproven,
                );
                if let Some(slot) = rejected_out.as_mut() {
                    **slot = Some(why);
                }
                false
            } else {
                match accept_or_restore_reasoned(
                    spec_path,
                    before,
                    snapshot,
                    step,
                    reporter,
                    &format!(
                        "\n## Attempt {attempt} — {label}\nReasoning: {}\n{}Confidence: {:?}\n",
                        outcome.reasoning,
                        evidence_lines(&outcome.evidence),
                        outcome.confidence
                    ),
                    notes_path.clone(),
                    &keep,
                ) {
                    Ok(()) => true,
                    Err(why) => {
                        if let Some(slot) = rejected_out.as_mut() {
                            **slot = Some(why);
                        }
                        false
                    }
                }
            };
            if !changed && !staged.is_empty() {
                append_notes(
                    notes_path.clone(),
                    &format!(
                        "\n## {label} — staged files kept\nThe edit was rolled back, but the \
                         files staged for this turn were kept: {}\n",
                        staged.join(", ")
                    ),
                );
            }
            if !changed && let Some(msg) = repeated_spec_rejection(spec_path) {
                return Err(format!("{label}: {msg}"));
            }
            // The turn cannot run `osc vc` (osc needs the keyring, which its
            // environment does not reach), so the entry is written here, from
            // what the turn said it did.
            if changed {
                if let Some(snap) = snapshot {
                    snap.restore_changes_files(reporter, Some(step));
                }
                for name in osc::drop_empty_rpmlintrc(checkout_dir) {
                    reporter.report(&Event::Warning {
                        step: Some(step.id),
                        message: format!("{rpm_name}: dropped the empty '{name}'"),
                    });
                }
                // One bullet per changelog line; the reasoning is prose, so only
                // its first sentence becomes the bullet.
                let source = if outcome.changelog.trim().is_empty() {
                    outcome
                        .reasoning
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    outcome.changelog.clone()
                };
                warn_if_err(
                    reporter,
                    "adding the .changes entry for the agent's edit",
                    osc::add_release_changes_entry(
                        checkout_dir,
                        &config.version,
                        &without_own_version(
                            &without_macros(
                                &changes_message(&source),
                                &config.pkg_name,
                                &config.version,
                            ),
                            &config.pkg_name,
                            &config.version,
                        ),
                        Emit::new(reporter, step.id),
                    ),
                );
            }
            Ok(if changed {
                TurnEffect::Edited
            } else {
                TurnEffect::NoChange
            })
        }
        crate::agent::Action::UseUpstreamWheel { why } => {
            // Minimum-shape gate on the reason: too short to be real
            // evidence rejects the switch outright instead of silently
            // swapping the recipe based on an empty `why`.
            if let Err(e) = validate_wheel_switch_why(why) {
                // A rejected switch is a rejected turn: the spec and the
                // checkout go back as they are for Blocked and NoChange,
                // keeping the staged files.
                if let Some(snap) = snapshot {
                    snap.restore_keeping(reporter, Some(step), &keep);
                }
                restore_spec(spec_path, before, Some(step), reporter);
                return Err(format!("{label}: {e}"));
            }
            // An npm failure in an sdist that ships a package-lock.json is
            // fixable with the `node_modules` OBS service. Send the agent
            // there once; a second `use_upstream_wheel` is honoured.
            if npm_vendor_route_pending(checkout_dir, &notes_path, &config.version) {
                // This arm returns without the acceptance gate, so the turn's
                // own edits are gated here, before the pipeline adds its files.
                // A rejection is rolled back (keeping the staged files) and the
                // turn is a no-op.
                let accepted = gate_turn_edit_without_accept(
                    spec_path,
                    before,
                    snapshot,
                    step,
                    reporter,
                    "\n## npm vendoring deferral — edits kept\n",
                    notes_path.clone(),
                    &keep,
                );
                if !accepted && let Some(msg) = repeated_spec_rejection(spec_path) {
                    return Err(format!("{label}: {msg}"));
                }
                // The pipeline does the file work of the route itself: the
                // lock file into the checkout and the `node_modules` service
                // entry. The turn only has to wire the spec to them.
                let staged = match stage_npm_vendor_route(checkout_dir, &config.version) {
                    Ok(done) => {
                        // The files the route wrote are the staged files of this
                        // turn: a later round of the ladder keeps them.
                        if let Some(out) = staged_out.as_mut() {
                            out.extend(NPM_VENDOR_FILES.iter().map(|f| f.to_string()));
                        }
                        format!("The pipeline already {}. ", done.join(" and "))
                    }
                    Err(e) => format!(
                        "The pipeline could not stage the lock file or the service ({e}): \
                         ask for it with `stage_files` and write the `_service` entry. "
                    ),
                };
                append_notes(
                    notes_path,
                    &format!(
                        "\n## {label} — use_upstream_wheel deferred\n\
                         {NPM_VENDOR_NOTE_MARKER} The sdist ships a package-lock.json, so \
                         the npm build can be done offline. {staged}Load the \
                         `npm-vendor-packaging` skill, add `Source10: package-lock.json` and \
                         the `#!CreateArchive` `node_modules.tar.gz` Source to the spec, and \
                         make the build hook reuse the vendored dependencies. Return \
                         `use_upstream_wheel` again only if that attempt fails.\n"
                    ),
                );
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: format!(
                        "{rpm_name}: npm build failure with a package-lock.json in the \
                         sdist; asking the agent to try npm vendoring before the wheel"
                    ),
                });
                return Ok(TurnEffect::NoChange);
            }
            reporter.report(&Event::Warning {
                step: Some(step.id),
                message: format!(
                    "{rpm_name} reported unbuildable from source in this environment \
                     ({why}); trying to repackage the upstream PyPI wheel instead"
                ),
            });
            match try_wheel_fallback_with(config, checkout_dir, spec_path, step, lookup) {
                Ok(wheel) => {
                    // Durable, reviewable audit record of what got swapped
                    // and why. See `write_wheel_switch_provenance` for the
                    // rationale — the OBS commit's reviewers do not see
                    // the run's JSONL, so the switch has to leave a trail
                    // inside the checkout itself.
                    if let Err(e) = write_wheel_switch_provenance(
                        rpm_name, config, &wheel, attempt, label, why, before,
                    ) {
                        reporter.report(&Event::Warning {
                            step: Some(step.id),
                            message: format!(
                                "{rpm_name}: wheel switch succeeded but writing \
                                 wheel-origin.txt / spec.pre-wheel-switch failed \
                                 ({e}); the switch is otherwise not recorded"
                            ),
                        });
                    }
                    // The snapshot is taken before the turn, so on rejection the
                    // wheel and rpmlintrc written above (not in it) are removed.
                    let changed = accept_or_restore_with_snapshot(
                        spec_path,
                        before,
                        snapshot,
                        step,
                        reporter,
                        &format!(
                            "\n## {label} — switched to upstream wheel\n\
                             Wheel: {}\n\
                             SHA256: {}\n\
                             Reasoning: {why}\n",
                            wheel.filename, wheel.sha256,
                        ),
                        notes_path,
                        &keep,
                    );
                    Ok(if changed {
                        TurnEffect::SwitchedToWheel
                    } else {
                        TurnEffect::NoChange
                    })
                }
                // A per-CPython-locked wheel set (Triton) is refused by the
                // automatic switch but IS packageable by hand (convention
                // 49). Failing the package here threw that away even when
                // the agent had correctly diagnosed it; hand the agent the
                // redirect through the notes it reads next turn and carry on.
                Err(e) if e.contains("no usable upstream wheel either") => {
                    // The arm returns NoChange without the acceptance gate, so
                    // the turn's edits are gated here. Either way the arm
                    // answers NoChange; a rejected edit is rolled back first.
                    gate_turn_edit_without_accept(
                        spec_path,
                        before,
                        snapshot,
                        step,
                        reporter,
                        "\n## use_upstream_wheel refused — edits kept\n",
                        notes_path.clone(),
                        &keep,
                    );
                    append_notes(
                        notes_path,
                        &format!(
                            "\n## {label} — use_upstream_wheel was refused\n\
                             {e}\n\
                             The automatic wheel switch cannot serve every flavor from one \
                             wheel. Do NOT return `use_upstream_wheel` again: if PyPI has a \
                             wheel for each flavor's CPython ABI, follow convention 49 and \
                             write the per-flavor wheel spec yourself with action `edited`; \
                             otherwise build that flavor from source or request what it needs (`blocked` is not accepted).\n"
                        ),
                    );
                    reporter.report(&Event::Warning {
                        step: Some(step.id),
                        message: format!(
                            "{rpm_name}: use_upstream_wheel refused ({e}); asking the \
                             agent to repackage per-flavor wheels by hand (convention 49)"
                        ),
                    });
                    Ok(TurnEffect::NoChange)
                }
                Err(e) => Err(format!("{label}: unbuildable from source ({why}), and {e}")),
            }
        }
    };

    // After any accepted agent edit, re-run OBS services so a newly-added or
    // renamed Source the agent fetches via a service — one it may have just
    // written a `_service` file for in this very turn, including on a
    // checkout that had none before — is actually materialised on disk
    // before the next validation sees it. Convention 13's "the checkout
    // does not gain a file just because the spec now names it correctly"
    // trip-up costs an entire extra attempt without this. Called
    // unconditionally rather than only when `_service` already existed,
    // because that gate would silently miss the case this exists for; `osc` itself is the authority on what to do when none
    // is present. Non-fatal: a service failure here surfaces as a warning
    // and lets the existing validation gate catch any actual breakage,
    // instead of masking whatever the agent was trying to fix behind an
    // unrelated service error.
    if matches!(
        &effect,
        Ok(TurnEffect::Edited | TurnEffect::SwitchedToWheel)
    ) && let Err(e) =
        run_service_keeping_version(checkout_dir, spec_path, Emit::new(reporter, step.id))
    {
        reporter.report(&Event::Warning {
            step: Some(step.id),
            message: format!("osc service ra after {label} failed: {e}; proceeding anyway"),
        });
    }

    effect
}

/// [`apply_turn_outcome`] plus the one side effect every one of its 6 call
/// sites in `run_package` must perform identically on `SwitchedToWheel`
/// (reset `sdist_top_level`/`from_repo_archive` — see that doc comment for
/// why). Four of the six needed nothing beyond that and were byte-identical;
/// the other two layer one extra statement (bumping `attempts`, or setting
/// `agent_edited`) on the returned `TurnEffect` — this is that shared base,
/// not a sixth copy of the same reset.
pub(super) fn apply_turn_and_reset(
    ctx: ApplyTurnCtx<'_>,
    outcome: &crate::agent::TurnOutcome,
    reporter: &dyn Reporter,
    sdist_top_level: &mut String,
    from_repo_archive: &mut bool,
) -> Result<TurnEffect, String> {
    apply_turn_and_reset_with(
        ctx,
        outcome,
        reporter,
        sdist_top_level,
        from_repo_archive,
        &fetch_release_json,
    )
}

/// [`apply_turn_and_reset`] with the PyPI lookup of a wheel switch given.
pub(super) fn apply_turn_and_reset_with(
    ctx: ApplyTurnCtx<'_>,
    outcome: &crate::agent::TurnOutcome,
    reporter: &dyn Reporter,
    sdist_top_level: &mut String,
    from_repo_archive: &mut bool,
    lookup: PypiLookup<'_>,
) -> Result<TurnEffect, String> {
    let effect = apply_turn_outcome_with(ctx, outcome, reporter, lookup)?;
    if effect == TurnEffect::SwitchedToWheel {
        *sdist_top_level = String::new();
        *from_repo_archive = false;
    }
    Ok(effect)
}

/// What happened to the spec as a result of [`apply_turn_outcome`]. See its
/// doc comment for why `SwitchedToWheel` needs to be distinguishable from
/// `Edited` rather than folded into one "changed" boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnEffect {
    NoChange,
    Edited,
    SwitchedToWheel,
}
