//! The sdist stage: unpacking the checkout's source tarball for the logs and the quilt link.

use super::*;
use crate::pipeline::archive::*;
use crate::pipeline::service_source::*;
use crate::pipeline::source_pass::*;

/// Step: FetchSdist, then the quilt `source/` symlink. The checkout's `_service`
/// runs first when it makes the source (see [`run_source_pass`]).
pub(super) fn fetch_sdist_stage(
    ctx: &RunCtx,
    st: &mut RunState,
    need_spec: bool,
) -> Result<(), PipelineResult> {
    let baseline = obs_baseline_dir(ctx.config).ok();
    let mut run =
        |dir: &Path, emit: Emit<'_>| run_checkout_service(ctx, dir, baseline.as_deref(), emit);
    fetch_sdist_stage_with(ctx, st, need_spec, &mut run)
}

/// [`fetch_sdist_stage`] with the service run given (see [`run_source_pass`]).
pub(super) fn fetch_sdist_stage_with(
    ctx: &RunCtx,
    st: &mut RunState,
    need_spec: bool,
    run: &mut dyn FnMut(&Path, Emit<'_>) -> Result<(), osc::Error>,
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

    // An obs_scm source is made by OBS when the package is built: the checkout has
    // no tarball to unpack, and a missing one is not a failure.
    if source_made_at_build_time(checkout_dir) {
        st.step_attempts.skip(
            reporter,
            StepKind::FetchSdist,
            SkipReason::Other("the source is made by OBS at build time".to_string()),
        );
        return Ok(());
    }

    // A CUDA component is NVIDIA's binary archive: there is no source tree to
    // read, and unpacking it would copy hundreds of megabytes for nothing.
    if crate::redist::is_redist_package(&ctx.config.rpm_name) {
        st.step_attempts.skip(
            reporter,
            StepKind::FetchSdist,
            SkipReason::Other("a redistributable archive is not unpacked".to_string()),
        );
        return Ok(());
    }

    // A checkout whose source a `_service` makes runs that service first: the
    // tarball is there to unpack below. (The service's own step is OscService.)
    let pass = run_source_pass(ctx, st, run)?;
    let step_attempts = &mut st.step_attempts;

    // ── Step: FetchSdist (extract for log analysis, if not done) ─────────────
    let tarball = find_tarball(checkout_dir, &ctx.config.version);
    let missing = tarball.is_none().then(|| classify_no_tarball(&pass));
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
            // `missing` is set whenever there is no tarball.
            let message = missing.map_or_else(String::new, |m| m.warning());
            reporter.report(&Event::Warning {
                step: Some(sdist_step.id),
                message,
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

    // The link follows what was unpacked: the name the archive gives its
    // directory is not always the tarball's stem.
    let top = unpacked_top_level(&ws.sdist_dir(), &st.sdist_top_level);
    if top != st.sdist_top_level {
        reporter.report(&Event::Progress {
            step: None,
            message: format!("the sdist unpacks into {top}/"),
        });
        st.sdist_top_level = top;
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
    // Every patch round works in this link: a checkout whose tarball is there
    // but whose link is not is stopped here, once, not in each round.
    if unpacked_source_missing(ctx) {
        return Err(ctx.failed(UNPACKED_SOURCE_MISSING.into()));
    }
    Ok(())
}

/// Whether the checkout's source should be unpacked at `ws.dir()/source` and
/// is not: the package has a tarball to unpack (not an OBS-made or redistributed
/// source), but no `source/` link (a dangling one counts as missing). A patch
/// round works in that link, so a missing one is a fault of the pipeline.
pub(super) fn unpacked_source_missing(ctx: &RunCtx) -> bool {
    if source_made_at_build_time(&ctx.checkout_dir)
        || crate::redist::is_redist_package(&ctx.config.rpm_name)
        || find_tarball(&ctx.checkout_dir, &ctx.config.version).is_none()
    {
        return false;
    }
    !ctx.ws.dir().join("source").exists()
}

/// The reason a patch round gives when [`unpacked_source_missing`] holds.
pub(super) const UNPACKED_SOURCE_MISSING: &str = "internal: the unpacked source link is missing";

/// The directory an extraction into `sdist_dir` is named by: `hint` when it
/// exists, else the one entry the extraction holds when that is a single
/// directory. An empty `hint` (a wheel unpacks flat) and an archive with
/// several top-level entries keep `hint`.
pub(super) fn unpacked_top_level(sdist_dir: &Path, hint: &str) -> String {
    if hint.is_empty() || sdist_dir.join(hint).exists() {
        return hint.to_string();
    }
    let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(sdist_dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect();
    match entries.as_slice() {
        [only] if only.file_type().is_ok_and(|t| t.is_dir()) => {
            only.file_name().to_string_lossy().into_owned()
        }
        _ => hint.to_string(),
    }
}
