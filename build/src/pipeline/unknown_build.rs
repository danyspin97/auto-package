//! The unknown-build gate of the generate stage: an archive whose build system
//! auto-package does not know keeps its carried recipe, or the run fails with the
//! reason. No spec is generated for it.

/// What the generate stage knows about an archive whose build system is
/// unknown. Its recipe is carried forward or the run fails: no spec is
/// generated for it, since a spec would be a guess at its build.
pub(super) struct UnknownBuild<'a> {
    /// `PipelineConfig::unknown_build_system`. Nothing is gated without it.
    pub unknown: bool,
    /// The archive the failure names: the repository, else the package.
    pub archive: &'a str,
    pub version: &'a str,
    /// The stage generates a spec in this run.
    pub need_spec: bool,
    /// The checkout had a spec before the stage ran.
    pub spec_existed: bool,
    /// `PipelineConfig::prefers_wheel`: the wheel is asked for.
    pub wheel_route: bool,
    /// The outcome of carrying the checkout's recipe forward, or `None` when
    /// it was not tried.
    pub carry: Option<Result<(), &'a str>>,
}

/// Whether a failed carry-forward keeps the checkout's recipe rather than
/// generating a spec over it: every package but a Python one, whatever its
/// archive's source (a forge's `source_repo`, or none for PyPI).
pub(super) fn keeps_recipe_on_failed_carry(rpm_name: &str) -> bool {
    !rpm_name.starts_with("python-")
}

/// `Ok` when the stage may go on, else the failure to report.
pub(super) fn unknown_build_gate(case: &UnknownBuild) -> Result<(), String> {
    if !case.unknown || !case.need_spec {
        return Ok(());
    }
    let fail = |reason: &str| {
        format!(
            "{}; {reason}",
            crate::sources::cannot_tell_message(case.archive, crate::sources::Refusal::Recipe)
        )
    };
    match case.carry {
        Some(Ok(())) => Ok(()),
        Some(Err(why)) => Err(fail(&format!(
            "the existing recipe could not be carried forward to {} ({why})",
            case.version
        ))),
        None if case.wheel_route && case.spec_existed => Err(fail(
            "the wheel route is not available for an unknown build system",
        )),
        None => Err(fail("there is no recipe to carry forward")),
    }
}
