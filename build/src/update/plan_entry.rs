//! The entry points of the update plan: the whole project, or the packages a
//! caller names, handed on one at a time (see [`plan_each`]).

use super::plan::{Watch, plan_each_watched};
use super::*;

/// Classify every package in `project` (optionally filtered to just `only`).
///
/// Hits OBS twice per package (`osc ls` to find the real spec filename, then
/// `osc cat <pkg> <picked>.spec`), plus one PyPI call per PyPI-sourced
/// package and one git-forge API call per repo-sourced one. Returned in
/// sorted RPM-name order so `--check` output is stable across runs. Fails
/// when the project's package listing cannot be read.
pub fn plan(project: &str, only: Option<&str>, emit: Emit<'_>) -> Result<Vec<Candidate>, String> {
    plan_where(
        project,
        &|n| only.is_none_or(|o| osc::project_key(o) == osc::project_key(n)),
        emit,
    )
}

/// [`plan`] over the packages of `project` that `wanted` accepts. Packages it
/// rejects are not fetched at all, which is what makes a few names out of a
/// large project cheap.
pub fn plan_where(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
) -> Result<Vec<Candidate>, String> {
    let mut candidates = Vec::new();
    plan_each(project, wanted, emit, &mut |c| {
        candidates.push(c);
        true
    })?;
    Ok(candidates)
}

/// [`plan_where`] handing each package to `on` as soon as it is classified,
/// so a caller can start on the first update while the rest are still being
/// checked. `on` returns whether to go on; `false` stops the sweep.
pub fn plan_each(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
    on: &mut dyn FnMut(Candidate) -> bool,
) -> Result<(), String> {
    plan_each_watched(
        project,
        wanted,
        emit,
        Watch {
            total: &|_| {},
            started: &|_| {},
        },
        on,
    )
}
