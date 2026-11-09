//! The validation round: validating a spec and the context of missing sources.

use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::archive::*;
use crate::pipeline::check_paths::*;
use crate::pipeline::commit::*;
use crate::pipeline::keep_version::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::spec_edits::*;
use crate::pipeline::update_mode::spec_changes::*;

/// Step: ValidateSpec. A rejected spec runs a fix turn and asks for another
/// round.
pub(super) fn validate_round(ctx: &RunCtx, st: &mut RunState) -> Result<Flow, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        obs_name: _,
        checkout_dir,
        spec_path,
        ..
    } = ctx;
    let reporter = *reporter;

    let validate_step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::ValidateSpec,
        None,
        &mut st.step_attempts,
        rpm_name.clone()
    );

    // Deterministic and idempotent, so safe to re-apply on every visit —
    // including a spec that already existed on disk and was never run
    // through `post_process` this invocation (`need_spec` was false).
    // Without this, each of these mistakes persists on every future run
    // (nothing else ever revisits an unchanged spec to fix it) and either
    // looks like a genuinely missing dependency (mixed-case PyPI name
    // written lowercase — `%{python_module cython >= 3}` RPM-expands to
    // the nonexistent `python313-cython`) or only surfaces as a runtime
    // failure well after a wasted build (a bare `%{$python_sitelib}`
    // outside `%python_expand`; `%python_install_alternative` without
    // its `Requires(post/postun): update-alternatives`).
    if let Ok(current) = std::fs::read_to_string(spec_path) {
        let unpacked = ws.dir().join("source");
        let origin = st.update.as_ref().and_then(|u| u.origin_spec.as_deref());
        let source = unpacked.is_dir().then_some(unpacked.as_path());
        // Bounds a distro package's newer release misses are dropped (the
        // dependency stays); see `relaxed_dependencies`.
        let relaxed = PkgState::load(ws).relaxed_dependencies;
        let passes = normalize_spec_text(&current, st.update.is_some(), origin, source)
            .apply("spec_with_published_defects_fixed", |s| {
                spec_with_published_defects_fixed(s, checkout_dir, &config.rpms_dir)
            })
            .apply("relax_dependencies", |s| {
                crate::spec::relax_dependencies(s, &relaxed)
            });
        if passes.text != current {
            write_or_warn(spec_path, &passes.text);
            report_changes(
                &passes.changes,
                rpm_name,
                Some(validate_step.id),
                reporter,
                ws.notes(),
            );
        }
    }

    // A patch that does not apply to the unpacked source is never dropped for
    // that: only one quilt proves already applied upstream is removed. Any
    // other stops the package for a human (an edit is rejected while one is
    // left, so no fix turn could help). A version bump has already given the
    // agent its rounds to rebase such a patch (`patch_stage`).
    if let Ok(current) = std::fs::read_to_string(spec_path) {
        match try_patches(spec_path, &current) {
            Ok(check) => {
                if let Some(note) = check.unchecked {
                    reporter.report(&Event::Warning {
                        step: Some(validate_step.id),
                        message: note,
                    });
                }
                if !check.failed.is_empty() {
                    let report = run_patch_check(ctx, crate::patch_check::Refresh::No);
                    let proven: Vec<String> = check
                        .failed
                        .iter()
                        .map(|(n, _)| n.clone())
                        .filter(|n| report.proves_applied(n))
                        .collect();
                    let lines = remove_proven_patches(ctx, validate_step.id, &proven);
                    if !lines.is_empty() {
                        append_notes(
                            ws.notes(),
                            &format!("\n## Patches (quilt)\n{}\n", lines.join("\n")),
                        );
                    }
                    let kept: Vec<String> = check
                        .failed
                        .iter()
                        .filter(|(n, _)| !proven.contains(n))
                        .map(|(n, why)| format!("{n}: {}", brief(why)))
                        .collect();
                    if !kept.is_empty() {
                        validate_step.finish(StepOutcome::Failed);
                        return Err(ctx.failed(format!(
                            "needs a human: {} patch(es) do not apply to the {} sources and were \
                             kept (a patch is removed only when quilt proves it already applied \
                             upstream): {}",
                            kept.len(),
                            config.version,
                            kept.join("; ")
                        )));
                    }
                }
            }
            Err(e) => reporter.report(&Event::Warning {
                step: Some(validate_step.id),
                message: format!("{rpm_name}: the patches were not checked: {e}"),
            }),
        }
    }

    match osc::rpmspec_check(spec_path, Emit::new(reporter, validate_step.id)) {
        Ok(expanded) => {
            // The fully expanded spec is the single most useful artifact
            // for diagnosing a spec problem — keep it rather than drop it.
            let artifact = validate_step.file("rpmspec.stdout");
            write_or_warn(&artifact, &expanded);
            reporter.report(&Event::Artifact {
                step: validate_step.id,
                name: "rpmspec.stdout".to_string(),
                path: artifact,
            });

            sync_quilt_series(&expanded, checkout_dir);

            let mut missing = missing_at_checkout(&expanded, checkout_dir);
            // A Source that is a file of the unpacked source (a lock file for the
            // `node_modules` service) is copied in here: the agent cannot.
            if !missing.is_empty() {
                let copied =
                    crate::spec::adopt_sources_from_tree(&missing, &ws.sdist_dir(), checkout_dir);
                if !copied.is_empty() {
                    ap_core::diag!(
                        "info: copied {} from the unpacked source into the checkout",
                        copied.join(", ")
                    );
                    missing = missing_at_checkout(&expanded, checkout_dir);
                }
            }
            // A `_service` (`download_files`, the one installed here) is how a Source
            // that is a URL gets onto disk — and the agent cannot run it.
            // Run it before calling the files missing, and keep its error
            // so the agent sees why a download did not happen.
            let mut service_error = None;
            // A Source URL with no `_service` at all gets the `download_files` one
            // written here; it is taken away again when it does not fetch, so the
            // checkout is as the agent left it.
            let generated = write_download_service(&expanded, &missing, checkout_dir);
            if !missing.is_empty() && checkout_dir.join("_service").is_file() {
                match run_service_keeping_version(
                    checkout_dir,
                    &config.rpm_name,
                    spec_path,
                    obs_baseline_dir(config).ok().as_deref(),
                    Emit::new(reporter, validate_step.id),
                ) {
                    Ok(()) => missing = missing_at_checkout(&expanded, checkout_dir),
                    Err(e) => {
                        service_error = Some(if generated {
                            format!("a generated `download_files` service failed: {e}")
                        } else {
                            e.to_string()
                        })
                    }
                }
                if generated && !missing.is_empty() {
                    let _ = std::fs::remove_file(checkout_dir.join("_service"));
                }
            }
            if !missing.is_empty() {
                let validate_step = validate_step.finish(StepOutcome::Failed);
                reporter.report(&Event::SpecRejected {
                    step: validate_step.id,
                    reason: ap_core::event::SpecRejection::MissingSourceFiles {
                        names: missing.clone(),
                    },
                    restored_from: spec_path.clone(),
                });

                if config.no_agent
                    || st.attempts >= config.max_attempts
                    || can_start(config.budget_usd, st.spent_usd, &ctx.signals).is_err()
                {
                    skip_agent(
                        reporter,
                        &mut st.step_attempts,
                        config,
                        st.attempts,
                        st.spent_usd,
                        &ctx.signals,
                    );
                    return Err(ctx.failed(format!(
                        "spec references source file(s) not present in the \
                         checkout: {}",
                        missing.join(", ")
                    )));
                }

                // Never commit a spec whose Source/Patch files don't exist — OBS
                // would spend a whole remote build just to report `File ...
                // SOURCES/...: No such file or directory` (convention 91 (section 13)).
                let context = missing_sources_context(&missing, service_error.as_deref());
                fix_after_validation(
                    ctx,
                    st,
                    Some(&context),
                    "missing source files",
                    "missing source files and agent failed: ",
                )?;
                return Ok(Flow::Retry);
            }

            // A skeleton left unfinished builds "successfully" and publishes an
            // empty package, so no spec with a TODO placeholder is committed
            // (convention 63).
            let todos = crate::spec::todo_placeholders(
                &std::fs::read_to_string(spec_path).unwrap_or_default(),
            );
            if !todos.is_empty() {
                let lines: Vec<String> = todos
                    .iter()
                    .map(|(n, text)| format!("{n}: {text}"))
                    .collect();
                let validate_step = validate_step.finish(StepOutcome::Failed);
                reporter.report(&Event::SpecRejected {
                    step: validate_step.id,
                    reason: ap_core::event::SpecRejection::Placeholders {
                        lines: lines.clone(),
                    },
                    restored_from: spec_path.clone(),
                });
                if config.no_agent
                    || st.attempts >= config.max_attempts
                    || can_start(config.budget_usd, st.spent_usd, &ctx.signals).is_err()
                {
                    skip_agent(
                        reporter,
                        &mut st.step_attempts,
                        config,
                        st.attempts,
                        st.spent_usd,
                        &ctx.signals,
                    );
                    return Err(ctx.failed(format!(
                        "the spec still has TODO placeholders: {}",
                        lines.join("; ")
                    )));
                }
                let context = format!(
                    "The spec still contains TODO placeholders, so it is an unfinished \
                     skeleton and cannot be built or committed (convention 63). Replace each \
                     with real content from `source/`: the summary, the SPDX license, the \
                     description, the build, install and check commands, and the exact \
                     `%files`. These are the lines:\n\n{}",
                    crate::agent::fenced_data(&lines.join("\n"))
                );
                fix_after_validation(
                    ctx,
                    st,
                    Some(&context),
                    "TODO placeholders",
                    "TODO placeholders and agent failed: ",
                )?;
                return Ok(Flow::Retry);
            }

            // A switch that tells an upstream build "I accept the terms" is a
            // person's decision; one that confirms a fact ("this is not a licence
            // violation") is allowed once the spec says what was checked. Either
            // way the package stops here unless the operator has said they
            // reviewed it.
            let guards = crate::spec::unverified_license_guards(&spec_text_now(spec_path));
            if !guards.is_empty() && !ap_core::env::env_flag("AUTO_PACKAGE_ALLOW_LICENSE_OVERRIDES")
            {
                let lines: Vec<String> = guards
                    .iter()
                    .map(|(n, text)| format!("{n}: {text}"))
                    .collect();
                validate_step.finish(StepOutcome::Failed);
                return Err(ctx.failed(format!(
                    "the spec sets a variable that overrides an upstream licence guard without \
                     a `# licence-guard-verified: <what was checked>` comment, or accepts terms, \
                     which needs a person's review before anything is built or committed ({}). \
                     Check what the guard asks about and record it in that comment, or review \
                     the licence and run again with AUTO_PACKAGE_ALLOW_LICENSE_OVERRIDES=1",
                    lines.join("; ")
                )));
            }

            // What a reviewer sends back at once: an emoji, a description that
            // only repeats the Summary, a `%files` with no `%license` although
            // the source ships a licence file (convention 63 (section 66), 5 (section 69)).
            let source = ws.dir().join("source");
            let mut style = style_problems(&spec_text_now(spec_path), &source);
            // On an update, a problem the origin's spec has too was inherited:
            // it is reported, not fixed here (see `update_mode::split_inherited`).
            if let Some(update) = carried_origin::inherit_mode(st) {
                let origin = update
                    .origin_spec
                    .as_deref()
                    .map_or_else(Vec::new, |o| style_problems(o, &source));
                let (introduced, inherited) = split_inherited(std::mem::take(&mut style), &origin);
                // The gate runs again after each fix: each problem is said once per run.
                for problem in newly_reported(&mut update.reported, inherited) {
                    reporter.report(&Event::Warning {
                        step: Some(validate_step.id),
                        message: format!(
                            "{rpm_name}: {problem}; the origin's spec has the same problem, \
                             so this update leaves it as it is"
                        ),
                    });
                }
                style = introduced;
            }
            style.extend(crate::spec::check_quality_problems(&spec_text_now(
                spec_path,
            )));
            {
                // The unpacked sources, either flat or under one top directory.
                let src = ws.dir().join("source");
                let tops: Vec<PathBuf> = std::fs::read_dir(&src)
                    .map(|d| {
                        d.flatten()
                            .map(|e| e.path())
                            .filter(|p| p.is_dir())
                            .collect()
                    })
                    .unwrap_or_default();
                if src.is_dir() {
                    // A path a spec patch creates is shipped once the patch is applied.
                    let created = patch_created_paths(
                        &spec_text_now(spec_path),
                        spec_path.parent().unwrap_or_else(|| Path::new("")),
                    );
                    let have = |p: &str| {
                        created.iter().any(|c| c == p)
                            || src.join(p).exists()
                            || tops.iter().any(|t| t.join(p).exists())
                    };
                    // The repository's tag archive as a second source, when the
                    // sdist metadata names the repository and the tag exists; the
                    // missing-source step above fetches it on the next pass.
                    let meta = |f: &str| {
                        std::iter::once(src.clone())
                            .chain(tops.iter().cloned())
                            .find_map(|d| std::fs::read_to_string(d.join(f)).ok())
                            .unwrap_or_default()
                    };
                    if let Some(fixed) = crate::spec::auto_fixture_source(
                        &spec_text_now(spec_path),
                        &have,
                        &meta("PKG-INFO"),
                        &meta("pyproject.toml"),
                        &config.version,
                        &|repo, v| crate::sources::tag_archive_url(repo, v),
                    ) {
                        write_or_warn(spec_path, &fixed);
                        validate_step.finish(StepOutcome::Ok);
                        return Ok(Flow::Retry);
                    }
                    // On an update, a missing path the origin's %check names too is
                    // inherited: reported, not a problem of this edit.
                    let origin =
                        carried_origin::inherit_mode(st).and_then(|u| u.origin_spec.clone());
                    let (introduced, inherited) = missing_check_path_split(
                        &spec_text_now(spec_path),
                        origin.as_deref(),
                        &have,
                    );
                    if let Some(update) = carried_origin::inherit_mode(st) {
                        for problem in newly_reported(&mut update.reported, inherited) {
                            reporter.report(&Event::Warning {
                                step: Some(validate_step.id),
                                message: format!(
                                    "{rpm_name}: {problem}; the origin's spec has the same \
                                     problem, so this update leaves it as it is"
                                ),
                            });
                        }
                    }
                    style.extend(introduced);
                }
            }
            if crate::spec::check_is_file_probe_only(&spec_text_now(spec_path))
                && ships_python_modules(&ws.dir().join("source"))
            {
                style.push(
                    "its %check only tests that a file exists, but the package ships Python \
                     modules: import them (and, for a wheel other packages import, the chain \
                     they import); `test -e` alone is for a package with no module \
                     (conventions 11 (section 67), 76)"
                        .to_string(),
                );
            }
            if !style.is_empty() {
                // Only style problems reach this point: every other problem
                // returned above. The step's outcome is decided after the fix
                // turn, so a style-only agent failure does not fail it.
                reporter.report(&Event::SpecRejected {
                    step: validate_step.id,
                    reason: ap_core::event::SpecRejection::Placeholders {
                        lines: style.clone(),
                    },
                    restored_from: spec_path.clone(),
                });
                if config.no_agent
                    || st.attempts >= config.max_attempts
                    || can_start(config.budget_usd, st.spent_usd, &ctx.signals).is_err()
                {
                    // Style alone never fails a package that otherwise builds.
                    ap_core::diag!(
                        "  not fixing spec style problems without an agent turn: {}",
                        style.join("; ")
                    );
                    validate_step.finish(StepOutcome::Failed);
                    return Ok(Flow::Proceed);
                }
                let context = format!(
                    "A reviewer would send this spec back before it is built:\n\n{}\n\n\
                     Fix each point (conventions 63 (section 66) and 5 (section 69)).",
                    style
                        .iter()
                        .map(|s| format!("- {s}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                // Once per package run: a turn that cannot satisfy a point must
                // not loop here.
                if st.step_attempts.round <= 1 {
                    return match fix_after_validation(
                        ctx,
                        st,
                        Some(&context),
                        "spec style",
                        "spec style and agent failed: ",
                    ) {
                        Ok(()) => {
                            validate_step.finish(StepOutcome::Failed);
                            Ok(Flow::Retry)
                        }
                        // Only a turn that could not run, gave no usable fix, or
                        // was blocked on every rung of the ladder is a warning.
                        // Budget exhaustion, cancellation and the apply gate's
                        // refusals still fail the package below. Every problem
                        // here is style, so an exhausted ladder is style too.
                        Err(PipelineResult::Failed { reason, kind, .. })
                            if matches!(
                                kind,
                                FailureKind::AgentUnavailable
                                    | FailureKind::AgentFailed
                                    | FailureKind::RateLimited
                                    | FailureKind::Busy
                                    | FailureKind::Transient
                                    | FailureKind::DiskFull
                                    | FailureKind::Exhausted
                            ) =>
                        {
                            // The ladder memo would refuse a later rebuild of
                            // this spec as spent; a style-only failure must not.
                            if kind == FailureKind::Exhausted {
                                update_state(ws, reporter, |state| {
                                    state.ladder_spec_hash = None;
                                    state.ladder_reason = None;
                                });
                            }
                            // Style is not worth failing a package that builds:
                            // the failure is a warning and the package proceeds.
                            ap_core::diag!(
                                "  warning: spec style fix failed, building the spec as it is: {reason}"
                            );
                            validate_step.finish(StepOutcome::Ok);
                            Ok(Flow::Proceed)
                        }
                        Err(other) => {
                            validate_step.finish(StepOutcome::Failed);
                            Err(other)
                        }
                    };
                }
                validate_step.finish(StepOutcome::Failed);
                return Ok(Flow::Proceed);
            }

            validate_step.finish(StepOutcome::Ok);
            Ok(Flow::Proceed)
        }
        Err(e) => {
            let validate_step = validate_step.finish(StepOutcome::Failed);
            write_or_warn(validate_step.file("rpmspec.stderr"), e.to_string());
            write_or_warn(ws.rpmspec_log(), e.to_string());
            reporter.report(&Event::SpecRejected {
                step: validate_step.id,
                reason: ap_core::event::SpecRejection::RpmspecParseFailed {
                    stderr: e.to_string(),
                },
                restored_from: spec_path.clone(),
            });
            if config.no_agent
                || st.attempts >= config.max_attempts
                || can_start(config.budget_usd, st.spent_usd, &ctx.signals).is_err()
            {
                skip_agent(
                    reporter,
                    &mut st.step_attempts,
                    config,
                    st.attempts,
                    st.spent_usd,
                    &ctx.signals,
                );
                return Err(ctx.failed(format!("spec invalid: {e}")));
            }

            // Never commit a spec rpm cannot parse — OBS would spend a
            // whole remote build to tell us what rpmspec just said. Fix it
            // here and re-validate on the next pass of the loop.
            fix_after_validation(
                ctx,
                st,
                None,
                "FixAfterValidation",
                "spec invalid and agent failed: ",
            )?;
            Ok(Flow::Retry)
        }
    }
}

/// The context handed to the agent when the spec names source files that are
/// not in the checkout.
///
/// The file names come from the spec (written by upstream, expanded from its
/// macros) and the service error is a command's output, so neither is
/// instructions: both go in blocks labelled as data that their own text cannot
/// close.
pub(super) fn missing_sources_context(missing: &[String], service_error: Option<&str>) -> String {
    let mut text = String::from(
        "`rpmspec --parse` succeeded, but the spec references source file(s) \
         that do not exist in the checkout directory (listed below, one per \
         line). Either the file was never fetched/written, or a Source/Patch \
         line's name doesn't match what's actually on disk (typo, wrong \
         extension, stale reference after a rename). Fix the spec to reference \
         the real file, or create/restore the missing file if it should exist \
         — see \"Source Files and Cleanup\" below.\n\n",
    );
    text.push_str(&crate::agent::untrusted_data(
        "missing-source-files",
        &missing.join("\n"),
    ));
    if let Some(e) = service_error {
        text.push_str(
            "\n\nThe checkout's `_service` was run (`osc service ra`) to fetch \
             it and failed:\n",
        );
        text.push_str(&crate::agent::untrusted_data("service-error", e));
    }
    text
}
