//! The sdist stage: unpacking the checkout's source tarball for the logs and the quilt link.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::service_source::*;

/// Step: FetchSdist, then the quilt `source/` symlink.
pub(super) fn fetch_sdist_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    need_spec: bool,
) -> Result<(), PipelineResult> {
    let RunCtx {
        pkg_run,
        reporter,
        ws,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let reporter = *reporter;
    let step_attempts = &mut st.step_attempts;

    // An obs_scm source is made by OBS when the package is built: the checkout has
    // no tarball to unpack, and a missing one is not a failure.
    if source_made_at_build_time(checkout_dir) {
        step_attempts.skip(
            reporter,
            StepKind::FetchSdist,
            SkipReason::Other("the source is made by OBS at build time".to_string()),
        );
        return Ok(());
    }

    // A CUDA component is NVIDIA's binary archive: there is no source tree to
    // read, and unpacking it would copy hundreds of megabytes for nothing.
    if crate::redist::is_redist_package(&ctx.config.rpm_name) {
        step_attempts.skip(
            reporter,
            StepKind::FetchSdist,
            SkipReason::Other("a redistributable archive is not unpacked".to_string()),
        );
        return Ok(());
    }

    // ── Step: FetchSdist (extract for log analysis, if not done) ─────────────
    let tarball = find_tarball(checkout_dir, &ctx.config.version);
    // A regenerated spec may come with a different source, so a leftover
    // extraction is not trusted then.
    if need_spec {
        warn_if_err(reporter, "clearing the old sdist", ws.clear_sdist());
    }
    let extracted = tarball
        .as_deref()
        .is_some_and(|t| ws.sdist_extracted_from(t));
    if !extracted {
        let sdist_step = begin_or_fail!(
            pkg_run,
            reporter,
            StepKind::FetchSdist,
            None,
            step_attempts,
            rpm_name.clone()
        );

        // Find tarball in checkout and extract to ws.sdist_dir()
        let mut outcome = StepOutcome::Ok;
        if let Some(tarball) = tarball {
            let sdist_dir = ws.sdist_dir();
            // A different tarball or a half-failed extraction left files here.
            warn_if_err(reporter, "clearing the old sdist", ws.clear_sdist());
            let _ = std::fs::create_dir_all(&sdist_dir);
            if let Err(e) = ap_ecosystem::sdist::extract(&tarball, &sdist_dir) {
                // The agent reads `sdist/` to review the package; an empty one
                // looks like "no sources" with no hint why.
                reporter.report(&Event::Warning {
                    step: Some(sdist_step.id),
                    message: format!("extracting {} failed: {e}", tarball.display()),
                });
                outcome = StepOutcome::Failed;
            } else {
                warn_if_err(
                    reporter,
                    "recording the sdist extraction",
                    ws.mark_sdist_extracted(&tarball),
                );
            }
        } else {
            reporter.report(&Event::Warning {
                step: Some(sdist_step.id),
                message: "no source tarball found in the checkout".to_string(),
            });
            outcome = StepOutcome::Failed;
        }

        sdist_step.finish(outcome);
    } else {
        step_attempts.skip(
            reporter,
            StepKind::FetchSdist,
            SkipReason::Other("sdist already extracted".to_string()),
        );
    }

    // ── Quilt setup: create source/ symlink ───────────────────────────────────
    // Keep this fresh every run so the symlink is correct even when the sdist
    // top-level dir changes (e.g. after --regen with a new version).
    #[cfg(unix)]
    {
        let source_link = ws.dir().join("source");
        let source_target = ws.sdist_dir().join(&st.sdist_top_level);
        if source_target.exists() {
            warn_if_err(
                reporter,
                "linking source/",
                ap_core::fsx::replace_symlink(&source_target, &source_link),
            );
        } else if source_link.symlink_metadata().is_ok() {
            // A link to a directory that is gone would mislead quilt.
            let _ = std::fs::remove_file(&source_link);
        }
    }
    Ok(())
}
