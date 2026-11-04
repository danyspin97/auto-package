//! The build results of the source package on OBS, against the target's own:
//! the reads the submit gate makes (`arch_check::gate_inputs`), judged the way
//! it does, each failure compared with the target's result for the same
//! repository and architecture.

use std::path::Path;

use super::fetch::RequestObs;
use super::gather::ActionInputs;
use super::{Finding, Severity, ids};
use crate::arch_check::{
    ArchObs, ArchState, BaselineState, GateRow, blocks_submit, gate_inputs, read_results,
};
use crate::obs_status::ResultEntry;

/// [`ArchObs`] over the review's reads. The two writes of the trait refuse,
/// so the arch check's code cannot change a project from a review.
pub struct ReadOnlyArchObs<'a>(pub &'a dyn RequestObs);

const READ_ONLY: &str = "read-only review";

impl ArchObs for ReadOnlyArchObs<'_> {
    fn project_meta(&self, _project: &str) -> Result<Option<String>, String> {
        Err(READ_ONLY.to_string())
    }

    fn set_project_meta(&self, _project: &str, _xml: &str) -> Result<(), String> {
        Err(READ_ONLY.to_string())
    }

    fn results(&self, project: &str, pkg: &str) -> Result<Vec<ResultEntry>, String> {
        Ok(self.0.results(project, pkg)?.unwrap_or_default())
    }

    fn results_if_present(
        &self,
        project: &str,
        pkg: &str,
    ) -> Result<Option<Vec<ResultEntry>>, String> {
        self.0.results(project, pkg)
    }

    fn buildlog(
        &self,
        _project: &str,
        _pkg: &str,
        _repo: &str,
        _arch: &str,
        _path: &Path,
    ) -> Result<(), String> {
        Err(READ_ONLY.to_string())
    }

    fn enable_repositories(
        &self,
        _project: &str,
        _pkg: &str,
        _repos: &[&str],
    ) -> Result<bool, String> {
        Err(READ_ONLY.to_string())
    }
}

/// The build findings. `rows` are the source's results with the baseline of
/// each failure, `target` the target's own results, and `stale` whether the
/// source moved on since the revision the request names (`None`: not known).
pub fn build_findings(
    package: &str,
    rows: &[GateRow],
    target: &[((String, String), ArchState)],
    stale: Option<bool>,
) -> Vec<Finding> {
    let mk = |id, sev, msg: &str| Finding::check(id, sev, package, msg);
    let mut out = Vec::new();
    if rows.is_empty() {
        out.push(mk(
            ids::BUILD_NO_RESULTS,
            Severity::Warning,
            "the source package has no build result on OBS",
        ));
    }
    for r in rows {
        let name = format!("{}/{}", r.repo, r.arch);
        // As the gate reads it: a failure without a baseline blocks.
        let baseline = r.baseline.as_ref().map_or(BaselineState::Builds, |b| {
            BaselineState::of(b.state.as_ref())
        });
        let detail = r.state.detail().map_or(String::new(), |d| format!(": {d}"));
        match &r.state {
            s if blocks_submit(s, baseline) => out.push(mk(
                ids::BUILD_FAILED,
                Severity::Blocker,
                &format!("{name} {}{detail}", s.label()),
            )),
            s if s.is_failure() => out.push(mk(
                ids::BUILD_FAILS_IN_TARGET_TOO,
                Severity::Note,
                &format!(
                    "{name} {} and {}{detail}",
                    s.label(),
                    if baseline == BaselineState::NoResult {
                        "has no result in the target project"
                    } else {
                        "fails in the target project too"
                    }
                ),
            )),
            ArchState::Pending(what) => out.push(mk(
                ids::BUILD_PENDING,
                Severity::Warning,
                &format!("{name} is still {what}"),
            )),
            ArchState::NotFinal(what) => out.push(mk(
                ids::BUILD_PENDING,
                Severity::Note,
                &format!("{name} port result is not final ({what})"),
            )),
            ArchState::NotTestable { detail } => out.push(mk(
                ids::BUILD_NOT_TESTABLE,
                Severity::Note,
                &format!(
                    "{name} port cannot be tested{}",
                    detail.as_ref().map_or(String::new(), |d| format!(": {d}"))
                ),
            )),
            s if s.is_skipped() => {
                let built_by_target = target.iter().any(|(k, t)| {
                    (k.0.as_str(), k.1.as_str()) == (r.repo.as_str(), r.arch.as_str())
                        && !t.is_skipped()
                });
                if built_by_target {
                    out.push(mk(
                        ids::BUILD_DISABLED_ARCH,
                        Severity::Note,
                        &format!(
                            "{name} is {} for the source and built by the target",
                            s.label()
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    if stale == Some(true) {
        out.push(mk(
            ids::BUILD_STALE,
            Severity::Warning,
            "the source package changed after the revision the request names: the results describe another revision",
        ));
    }
    out
}

/// Read the results and judge them. A result that cannot be read, the
/// source's or the target's, is an error: the review cannot say whether the
/// package builds, and must not report that it does.
pub fn build_report(obs: &dyn RequestObs, a: &ActionInputs) -> Result<Vec<Finding>, String> {
    let package = a.target.package.as_str();
    let read_only = ReadOnlyArchObs(obs);
    let rows = gate_inputs(
        &read_only,
        &a.source.project,
        &a.target.project,
        &a.source.package,
    )
    .map_err(|e| format!("the build results could not be read: {e}"))?;
    let target = read_results(&read_only, &a.target.project, package)
        .map_err(|e| format!("the build results of the target could not be read: {e}"))?;
    let stale = match (&a.source_srcmd5, &a.current_srcmd5) {
        (Some(at), Some(now)) => Some(at != now),
        _ => None,
    };
    Ok(build_findings(package, &rows, &target, stale))
}
