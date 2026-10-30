//! The generate stage: the spec generation, the OSC service run, the sdist fetch
//! and the budget a resumed run keeps.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::carry::*;
use crate::pipeline::commit::*;
use crate::pipeline::gen_spec::*;
use crate::pipeline::npm_vendor::*;
use crate::pipeline::prompt_notes::*;
use crate::pipeline::render::*;
use crate::pipeline::rpm_deps::*;
use crate::pipeline::tarball::*;

/// Step: GenerateSpec (if needed). Sets `sdist_top_level` and
/// `from_repo_archive` on the state.
pub(super) fn generate_spec_stage(
    ctx: &RunCtx,
    st: &mut RunState,
) -> Result<SpecInfo, PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        ws,
        rpm_name,
        obs_name,
        checkout_dir,
        spec_path,
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;

    // Captured before generation can overwrite the spec: "did this package
    // already have a spec" must reflect what was checked out, not what we
    // are about to write.
    let spec_existed_before_generation = spec_path.exists();
    let version_bump =
        spec_existed_before_generation && spec_version_differs(spec_path, &config.version);
    // A wheel is asked for and the checked-out recipe builds from source.
    let wheel_switch = wheel_policy_pending(&config.source_urls, &config.pkg_name, spec_path);
    let need_spec = config.regen || !spec_existed_before_generation || version_bump || wheel_switch;
    // A bump keeps the recipe the package already has (its patches,
    // BuildRequires, `%check` skips, a distro package's whole layout) unless
    // `--regen` asks to start over; only the version and the source change.
    let old_version = spec_existed_before_generation
        .then(|| std::fs::read_to_string(spec_path).ok())
        .flatten()
        .and_then(|t| crate::spec::parse_header(&t))
        .map(|h| h.version);
    let carry_forward = version_bump
        && !config.regen
        && !config.prefers_wheel()
        && !crate::redist::is_redist_package(&config.rpm_name);
    let mut carried_from: Option<String> = None;

    let (sdist_top_level, is_maturin, from_repo_archive, is_rust) = if need_spec {
        let step = begin_or_fail!(
            pkg_run,
            reporter,
            StepKind::GenerateSpec,
            None,
            step_attempts,
            rpm_name.clone()
        );

        let generated = if carry_forward {
            match carry_forward_spec(config, checkout_dir, spec_path, &step) {
                Ok(res) => {
                    carried_from = old_version.clone();
                    Ok(res)
                }
                Err(why) => {
                    // A generated spec starts from scratch: it would drop what a
                    // maintained C/C++ recipe has (noarch, its Source URL, a
                    // multibuild, patches). That is only done when asked for
                    // (`--regen`); Python packages keep the old fallback, as a
                    // generated spec is what they are normally made from.
                    if config.source_repo.is_some() && !config.rpm_name.starts_with("python-") {
                        step.finish(StepOutcome::Failed);
                        return Err(ctx.failed(format!(
                            "could not carry the existing recipe forward to {} ({why}); a new \
                             spec would replace the package's own recipe, so nothing was \
                             changed (pass --regen to generate one anyway)",
                            config.version
                        )));
                    }
                    reporter.report(&Event::Warning {
                        step: Some(step.id),
                        message: format!(
                            "could not carry the existing recipe forward to {} ({why}); \
                             generating a new spec instead",
                            config.version
                        ),
                    });
                    generate_spec(config, checkout_dir, spec_path, &step)
                }
            }
        } else {
            generate_spec(config, checkout_dir, spec_path, &step)
        };
        match generated {
            Ok(res) => {
                let step = step.finish(StepOutcome::Ok);
                // "Initial packaging" is only true for a package that had no
                // spec before this run generated one — a version bump or
                // --regen on an already-packaged package must not be
                // (mis)recorded as an initial changelog entry. `create_initial_changes`
                // itself also skips if a `.changes` file already exists, so
                // either signal of "not actually new" is enough to skip.
                if !spec_existed_before_generation {
                    warn_if_err(
                        reporter,
                        "creating the initial .changes entry",
                        osc::create_initial_changes(
                            checkout_dir,
                            obs_name,
                            Emit::new(reporter, step.id),
                        ),
                    );
                }
                // A version bump on an already-packaged package gets its own
                // `.changes` entry; without one OBS accepts the new sources
                // with no record of why they changed.
                if version_bump {
                    warn_if_err(
                        reporter,
                        "adding the version-update .changes entry (the package ships \
                         without an entry recording the update, and OBS and a submit \
                         request will show none)",
                        osc::add_changes_entry(
                            checkout_dir,
                            &format!("Update to {}", config.version),
                            Emit::new(reporter, step.id),
                        ),
                    );
                    warn_if_err(
                        reporter,
                        "adding the upstream recap to the update .changes entry",
                        crate::release_notes::add_recap(
                            checkout_dir,
                            config.source_repo.as_deref(),
                            old_version.as_deref(),
                            &config.version,
                        ),
                    );
                }
                // Link spec.generated into the workspace so the review agent can compare.
                #[cfg(unix)]
                {
                    let src = step.file("spec.generated");
                    let link = ws.dir().join("spec.generated");
                    warn_if_err(
                        reporter,
                        "linking spec.generated",
                        ap_core::fsx::replace_symlink(&src, &link),
                    );
                }
                (
                    res.sdist_top_level,
                    res.is_maturin,
                    res.from_repo_archive,
                    res.is_rust,
                )
            }
            Err(GenSpecError::WheelOnly {
                pkg_name,
                version,
                candidates,
            }) => {
                // The pipeline stops here, so the step did not produce a spec.
                let step = step.finish(StepOutcome::Failed);

                let mut message = format!(
                    "{pkg_name} {version} is published on PyPI as wheels only and its \
                     metadata names no source repository."
                );

                if !candidates.is_empty() {
                    message.push_str(&format!(
                        "\n\nAuto-discovered {} candidate repositories:\n{}",
                        candidates.len(),
                        candidates
                            .iter()
                            .map(|url| format!("  - {}", url))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                    message.push_str(
                        "\n\nThese are suggestions - use WebSearch to verify the correct repository.\n\
                         Once confirmed, report the URL in your response."
                    );
                } else {
                    message.push_str(
                        "\n\nNo candidate repositories auto-discovered. Use WebSearch to find \
                         the source repository.",
                    );
                }

                message.push_str(&format!(
                    "\n\nAlternatively, provide it manually with:\n\
                     \x20   --source-url {pkg_name}=https://github.com/<owner>/<repo>"
                ));

                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: message.clone(),
                });

                // TODO: Call agent to validate candidates and find repository
                // For now, mark as wheel-only and skip
                update_state(ws, reporter, |state| {
                    state.last_built_version = Some(version);
                    state.last_outcome = Some("wheel-only".to_string());
                    state.last_failure_reason = None;
                });
                return Err(ctx.failed(format!(
                    "wheel-only package - candidates: {}",
                    if !candidates.is_empty() {
                        candidates.join(", ")
                    } else {
                        "none found, needs WebSearch".to_string()
                    }
                )));
            }
            Err(GenSpecError::Other(e)) => {
                let step = step.finish(StepOutcome::Failed);
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: e.clone(),
                });
                // A download that failed on the network is not the package's fault.
                return Err(ctx.failed_text(osc::text_is_transient(&e), e));
            }
            Err(GenSpecError::DiskFull(e)) => {
                let step = step.finish(StepOutcome::Failed);
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: e.clone(),
                });
                return Err(ctx.failed_as(FailureKind::DiskFull, e));
            }
            Err(GenSpecError::NoReleaseArchive(e)) => {
                let step = step.finish(StepOutcome::Failed);
                reporter.report(&Event::Warning {
                    step: Some(step.id),
                    message: e.clone(),
                });
                return Err(ctx.failed(e));
            }
        }
    } else {
        // Sdist may already be extracted; find top-level dir name from the tarball
        // in the checkout dir (or fall back to a reasonable guess).
        let top = find_sdist_top_level(checkout_dir, &config.pkg_name, &config.version);
        step_attempts.skip(reporter, StepKind::GenerateSpec, SkipReason::SpecExists);
        // A spec that cannot be read says nothing about Rust or maturin: the
        // read error is a transient failure, not a reason to build without
        // the Rust or maturin setup.
        let spec_text = match std::fs::read_to_string(spec_path) {
            Ok(text) => text,
            Err(e) => {
                return Err(ctx.failed_text(true, format!("reading the spec: {e}")));
            }
        };
        let is_maturin = spec_text.contains("maturin");
        let is_rust = spec_text.contains("cargo_build");
        (top, is_maturin, false, is_rust)
    };
    st.sdist_top_level = sdist_top_level;
    st.from_repo_archive = from_repo_archive;
    Ok(SpecInfo {
        need_spec,
        is_maturin,
        is_rust,
        carried_from,
    })
}

/// Steps: OscService for Rust crates and for maturin packages.
pub(super) fn osc_service_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    spec: &SpecInfo,
) -> Result<(), PipelineResult> {
    let RunCtx {
        config,
        pkg_run,
        reporter,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;
    let SpecInfo {
        need_spec,
        is_maturin,
        is_rust,
        ..
    } = *spec;

    // ── Step: OscService (Rust crates) ───────────────────────────────────────
    // `tar_scm` fetches the tag and `cargo_vendor` produces vendor.tar.zst, the
    // two sources the Rust spec template references. It has to run before the
    // commit, not before spec generation — the spec is a template either way.
    // A maturin package writes its own `_service` below, which replaces this
    // one: running `tar_scm` first would only leave its output unreferenced.
    if is_rust
        && !is_maturin
        && let Some(ref repo_url) = config.source_repo
    {
        let need_service = need_spec || !checkout_dir.join("_service").exists();
        if need_service {
            let step = begin_or_fail!(
                pkg_run,
                reporter,
                StepKind::OscService,
                None,
                step_attempts,
                rpm_name.clone()
            );

            let service_xml = render_service_file(repo_url, &config.version, &config.pkg_name);
            let result = ap_core::fsx::write_atomic(&checkout_dir.join("_service"), &service_xml)
                .map_err(|e| {
                    (
                        false,
                        format!("writing _service failed for {}: {e}", config.pkg_name),
                    )
                })
                .and_then(|()| {
                    run_service_with_retries(checkout_dir, Emit::new(reporter, step.id))
                        .map_err(|e| (e.is_transient(), e.to_string()))
                });

            if let Err((transient, reason)) = result {
                return Err(ctx.failed_text(transient, reason));
            }
            step.finish(StepOutcome::Ok);
        } else {
            step_attempts.skip(reporter, StepKind::OscService, SkipReason::ServiceUpToDate);
        }
    } else if is_rust && !is_maturin {
        // Without a repository there is nothing for `tar_scm` to fetch; say so
        // rather than leave the step silently absent.
        step_attempts.skip(
            reporter,
            StepKind::OscService,
            SkipReason::Other("package has no source repository".to_string()),
        );
    }

    // ── Step: OscService (maturin packages) ──────────────────────────────────
    if is_maturin {
        // The crate maturin actually builds is `[tool.maturin] manifest-path`,
        // which is often NOT the root Cargo.toml (hf_xet: the root is a
        // workspace that EXCLUDES hf_xet/, whose own Cargo.lock holds `pprof`).
        // Vendoring only the root manifest leaves the registry short of the
        // build's crates: "no matching package named `pprof` found".
        let extra_manifests = find_tarball(checkout_dir, &config.version)
            .and_then(|t| match maturin_manifest_path(&t) {
                Ok(path) => path,
                Err(e) => {
                    // Without it the vendored registry may lack the crates the
                    // build needs; say why instead of vendoring the root only.
                    reporter.report(&Event::Warning {
                        step: None,
                        message: format!(
                            "could not read the maturin manifest path from {}: {e}",
                            t.display()
                        ),
                    });
                    None
                }
            })
            .into_iter()
            .collect::<Vec<_>>();
        let service_lacks_manifest = std::fs::read_to_string(checkout_dir.join("_service"))
            .map(|svc| {
                extra_manifests
                    .iter()
                    .any(|m| !svc.contains(crate::obs_xml::escape(m).as_str()))
            })
            .unwrap_or(false);
        let need_service =
            need_spec || !checkout_dir.join("registry.tar.zst").exists() || service_lacks_manifest;
        if need_service {
            let step = begin_or_fail!(
                pkg_run,
                reporter,
                StepKind::OscService,
                None,
                step_attempts,
                rpm_name.clone()
            );

            // Fix any broken workspace definitions in the tarball before vendoring
            if let Some(tarball) = find_tarball(checkout_dir, &config.version) {
                warn_if_err(
                    reporter,
                    "fixing the cargo workspace in the tarball",
                    fix_cargo_workspace_in_tarball(&tarball, &config.pkg_name),
                );
            }

            let normalized = config.pkg_name.replace('-', "_").to_lowercase();
            let locked = find_tarball(checkout_dir, &config.version)
                .is_some_and(|t| tarball_has_cargo_lock(&t, &extra_manifests));
            // A sdist whose crate lives in a subdirectory (safetensors:
            // `bindings/python`) has no root manifest; naming one makes
            // `obs-service-cargo` fail with "Path to manifest is not a file".
            let root_manifest = find_tarball(checkout_dir, &config.version)
                .is_none_or(|t| tarball_has_root_cargo_toml(&t));
            let service_xml =
                render_maturin_service_file(&normalized, &extra_manifests, locked, root_manifest);
            let result = ap_core::fsx::write_atomic(&checkout_dir.join("_service"), &service_xml)
                .map_err(|e| {
                    (
                        false,
                        format!("writing _service failed for {}: {e}", config.pkg_name),
                    )
                })
                .and_then(|()| {
                    run_service_with_retries(checkout_dir, Emit::new(reporter, step.id))
                        .map_err(|e| (e.is_transient(), e.to_string()))
                });

            if let Err((transient, reason)) = result {
                return Err(ctx.failed_text(transient, reason));
            }
            step.finish(StepOutcome::Ok);
        } else {
            step_attempts.skip(reporter, StepKind::OscService, SkipReason::ServiceUpToDate);
        }
    }
    if !is_rust && !is_maturin {
        step_attempts.skip(reporter, StepKind::OscService, SkipReason::NotRust);
    }
    Ok(())
}

/// Resume cumulative spend AND the fix-attempt count from state.json ONLY
/// for an outer-loop retry inside the current CLI run (same run_id), so
/// both the dollar budget and `max_attempts` are enforced across the
/// retries of one package during one invocation — a package that comes
/// back through `run_package` again later in the same run (e.g. after a
/// `MissingDeps` round trip) must not get a completely fresh attempt
/// budget on top of what it already burned. A fresh CLI invocation
/// (different run_id) starts the package at $0/0 attempts — the user
/// asked for the restart-reset explicitly, and it matches the mental
/// model that a new command is a new budget. See
/// `PkgState::cumulative_spent_run_id`.
pub(super) fn resume_budget(ctx: &RunCtx, st: &mut RunState) -> Result<(), PipelineResult> {
    let config = ctx.config;
    // A state file that cannot be read must not reset the budget: the spend
    // of an earlier attempt would be forgotten. The read is retried later.
    let prior = PkgState::try_load(&ctx.ws).map_err(|e| {
        ctx.failed_text(
            true,
            format!("cannot read the package state, so the budget is unknown: {e}"),
        )
    })?;
    let resuming_this_run =
        prior.cumulative_spent_run_id.as_deref() == Some(config.run_id.as_str());
    st.attempts = if resuming_this_run {
        prior.cumulative_attempts
    } else {
        0
    };
    st.rpmlint_shown = load_rpmlint_shown(&prior, &config.version);
    st.spent_usd = if resuming_this_run {
        prior.cumulative_spent_usd
    } else {
        0.0
    };
    let (spent_usd, attempts, rpm_name) = (st.spent_usd, st.attempts, &ctx.rpm_name);
    if spent_usd > 0.0 || attempts > 0 {
        ctx.reporter.report(&Event::Progress {
            step: None,
            message: format!(
                "{rpm_name}: resuming with ${spent_usd:.2} spent and {attempts} fix \
                 attempt(s) already used on this package in this CLI run (budget cap \
                 ${:.2}, max {} attempts)",
                config.budget_usd, config.max_attempts
            ),
        });
    }
    Ok(())
}
