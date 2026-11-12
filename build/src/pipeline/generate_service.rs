//! The OscService stage.

use super::*;
use crate::pipeline::commit::*;
use crate::pipeline::npm_vendor::*;
use crate::pipeline::recap_entry::*;
use crate::pipeline::render::*;
use crate::pipeline::source_pass::pass_will_run;
use crate::pipeline::tarball::*;

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

            // tar_scm writes the changelog entry with this author, so one that
            // cannot be resolved stops a run that commits before anything is
            // written; a local build commits nothing and writes no entry.
            let author = super::author_gate::service_author(
                osc::local_build().is_some(),
                super::author_gate::host_author(Some(checkout_dir)),
            )
            .map_err(|why| ctx.failed_as(FailureKind::Environment, why))?;
            let service_xml = render_service_file(
                repo_url,
                &config.version,
                &config.pkg_name,
                author.map(|a| a.to_string()).as_deref(),
            );
            let result = ap_core::fsx::write_atomic(&checkout_dir.join("_service"), &service_xml)
                .map_err(|e| {
                    ctx.failed(format!(
                        "writing _service failed for {}: {e}",
                        config.pkg_name
                    ))
                })
                .and_then(|()| {
                    run_service_with_retries(
                        checkout_dir,
                        &config.rpm_name,
                        obs_baseline_dir(config).ok().as_deref(),
                        Emit::new(reporter, step.id),
                    )
                    .map_err(|e| ctx.failed_osc_text(&e))
                });

            result?;
            step.finish(StepOutcome::Ok);
            add_new_package_recap(ctx, spec.new_package);
        } else {
            // The source pass runs an existing `_service` that makes the source
            // (see `source_pass`), before the sdist is unpacked.
            let reason = if pass_will_run(checkout_dir, config) {
                SkipReason::Other("the existing _service runs before unpacking".to_string())
            } else {
                SkipReason::ServiceUpToDate
            };
            step_attempts.skip(reporter, StepKind::OscService, reason);
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
                    ctx.failed(format!(
                        "writing _service failed for {}: {e}",
                        config.pkg_name
                    ))
                })
                .and_then(|()| {
                    run_service_with_retries(
                        checkout_dir,
                        &config.rpm_name,
                        obs_baseline_dir(config).ok().as_deref(),
                        Emit::new(reporter, step.id),
                    )
                    .map_err(|e| ctx.failed_osc_text(&e))
                });

            result?;
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
