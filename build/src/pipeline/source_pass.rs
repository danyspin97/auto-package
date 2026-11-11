//! The source pass: the checkout's `_service` runs before the sdist is unpacked
//! when that service is what makes the source tarball.
//!
//! An existing package keeps its `_service`, so the OscService stage skips it
//! as up to date, and a checkout with no tarball needs that service to run
//! here, before FetchSdist.

use super::*;
use crate::obs_xml::{mask_xml_comments, service_elements};
use crate::pipeline::archive::find_tarball;
use crate::pipeline::commit::retry_transient;
use crate::pipeline::keep_version::run_service_keeping_version;
use crate::pipeline::service_source::*;

/// Attempts and pause of the pass's service run. The same as
/// `run_service_with_retries` (commit.rs), which cannot be shared: it runs the
/// service without keeping `Version:`.
const PASS_ATTEMPTS: u32 = 3;
const PASS_PAUSE: std::time::Duration = std::time::Duration::from_secs(20);

/// What the source pass did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SourcePass {
    /// No pass was needed (see [`source_pass_needed`]).
    NotRun,
    /// The service ran and exited cleanly.
    Ran,
    /// The service ran and failed: its error text.
    Failed(String),
}

/// Whether the checkout's source is made by a service that must run before the
/// sdist is unpacked: the `_service` of `service` (its text, empty without one)
/// fetches the source (`tar_scm`, or `cargo_vendor` with a `url`) or lists
/// `download_files`, and no tarball is in the checkout yet. An `obs_scm` source
/// is made by OBS at build time, a redistributed archive is not unpacked, and a
/// checkout that has its tarball needs no pass.
pub(super) fn source_pass_needed(service: &str, tarball_present: bool, redist: bool) -> bool {
    !redist && !tarball_present && makes_source_by_service(service)
}

/// Whether the `_service` text `service` makes the source when it runs: a
/// `tar_scm` or `cargo_vendor` with a `url` (see `source_service`), or a
/// `download_files`. An `obs_scm` source is made at build time instead.
pub(super) fn makes_source_by_service(service: &str) -> bool {
    let masked = mask_xml_comments(service);
    match source_service(&masked) {
        Some((_, SourceService::BuildTime)) => false,
        Some((_, SourceService::Fetched)) => true,
        None => !service_elements(&masked, "download_files").is_empty(),
    }
}

/// Runs the pass with `run` as the service run: called with the checkout and
/// the emitter of the OscService step it opens, once, when
/// [`source_pass_needed`] holds. A failure of the service is recorded on the
/// step and returned as [`SourcePass::Failed`]; the run goes on, and validation
/// shows the error to the agent. A failure of the machine (no `osc` to launch,
/// a full disk) stops the run as the OscService stage does.
pub(super) fn run_source_pass(
    ctx: &RunCtx,
    st: &mut RunState,
    run: &mut dyn FnMut(&Path, Emit<'_>) -> Result<(), osc::Error>,
) -> Result<SourcePass, PipelineResult> {
    let RunCtx {
        pkg_run,
        reporter,
        rpm_name,
        checkout_dir,
        ..
    } = ctx;
    let reporter = *reporter;
    let service = checkout_service(checkout_dir);
    let tarball_present = find_tarball(checkout_dir, &ctx.config.version).is_some();
    let redist = crate::redist::is_redist_package(&ctx.config.rpm_name);
    if !source_pass_needed(&service, tarball_present, redist) {
        return Ok(SourcePass::NotRun);
    }

    let step = begin_or_fail!(
        pkg_run,
        reporter,
        StepKind::OscService,
        None,
        &mut st.step_attempts,
        rpm_name.clone()
    );
    let id = step.id;
    match run(checkout_dir, Emit::new(reporter, id)) {
        Ok(()) => {
            step.finish(StepOutcome::Ok);
            Ok(SourcePass::Ran)
        }
        Err(e) if is_environment(&e) => {
            step.finish(StepOutcome::Failed);
            Err(ctx.failed_osc(&e))
        }
        Err(e) => {
            reporter.report(&Event::Warning {
                step: Some(id),
                message: format!("the _service failed: {e}"),
            });
            step.finish(StepOutcome::Failed);
            Ok(SourcePass::Failed(e.to_string()))
        }
    }
}

/// The service run of the pass in production: `osc service` on the checkout,
/// keeping the spec's `Version:`, retried when the network failed it.
pub(super) fn run_checkout_service(
    ctx: &RunCtx,
    dir: &Path,
    baseline: Option<&Path>,
    emit: Emit<'_>,
) -> Result<(), osc::Error> {
    retry_transient(PASS_ATTEMPTS, PASS_PAUSE, osc::Error::is_transient, || {
        run_service_keeping_version(dir, &ctx.rpm_name, &ctx.spec_path, baseline, emit)
    })
}

/// Whether `e` says the machine cannot run the service at all: no `osc` to
/// launch, or a full disk. Those are not the package's, and stop the run.
fn is_environment(e: &osc::Error) -> bool {
    use crate::proc::ProcessError;
    e.is_disk_full()
        || matches!(
            e,
            osc::Error::Launch(_)
                | osc::Error::Process(ProcessError::Launch { .. })
                | osc::Error::Command {
                    source: ProcessError::Launch { .. },
                    ..
                }
        )
}

/// What a checkout with no tarball means, given what the pass did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NoTarball {
    /// The services ran and made no tarball; the service's error when it failed.
    ServicesRan(Option<String>),
    /// No service known to make one: the pass did not run.
    Unmade,
}

/// The [`NoTarball`] outcome of a checkout after a pass that did `pass`. The
/// pass runs for every checkout that needs one (see `pass_will_run`), so a
/// checkout whose pass did not run has no service to wait for.
pub(super) fn classify_no_tarball(pass: &SourcePass) -> NoTarball {
    match pass {
        SourcePass::NotRun => NoTarball::Unmade,
        SourcePass::Ran => NoTarball::ServicesRan(None),
        SourcePass::Failed(e) => NoTarball::ServicesRan(Some(e.clone())),
    }
}

impl NoTarball {
    /// The FetchSdist warning.
    pub(super) fn warning(&self) -> String {
        match self {
            NoTarball::ServicesRan(Some(e)) => {
                format!("the _service ran but made no source tarball: {e}")
            }
            NoTarball::ServicesRan(None) => "the _service ran but made no source tarball".into(),
            NoTarball::Unmade => {
                "no source tarball found in the checkout, and no _service known to make one".into()
            }
        }
    }
}
