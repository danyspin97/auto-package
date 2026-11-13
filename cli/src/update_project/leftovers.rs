//! The cheap pre-gate of `lint` for a package that carries unpublished work:
//! a branch whose version is ahead of its origin (see `existing_lint`), or a
//! `--in-place` package that is a link whose sources differ from its origin.
//! The work is reviewed against its origin without downloads and without a
//! plan, from the audit thread. A finding the tool fixes sends the package to
//! the tidy path even when the audit found nothing; a finding the agent fixes
//! is handed to the agent with the package's findings.

use super::existing::scheme_of_ecosystem;
use super::existing_lint::{Continuation, decide_versions, origin_version};
use super::*;
use ap_build::bugfix::{FindingRoute, ReviewFinding, route};
use ap_build::request_review::fetch::LiveRequestObs;
use ap_build::request_review::gather::ActionInputs;
use ap_build::request_review::provenance_check::NO_FORGE;
use ap_build::request_review::run::{Deps, Options};
use ap_build::request_review::synthetic::review_branch;
use ap_build::request_review::{Finding, Severity, ids};
use ap_build::update::Candidate;
use ap_build::update_project::branch_version;

/// The reason a lookup of the pre-pass is not made.
const NOT_IN_PRE_PASS: &str = "not checked in the pre-pass";

/// A package whose unpublished work is compared with its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Carry {
    /// The project that holds the work: the branch, or the source itself for `--in-place`.
    pub(super) work: String,
    /// The origin the work is reviewed against.
    pub(super) origin: String,
    pub(super) origin_pkg: String,
}

/// What the pre-pass found for a carrying package, one line per finding.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct PreGate {
    /// Findings the tool fixes (a dependency bullet, a leftover, a header, a stale FLAG).
    pub(super) tool: Vec<String>,
    /// Findings the agent fixes.
    pub(super) agent: Vec<String>,
    /// The ids of the findings the tool does not fix and that stop the package
    /// from being ready (see `ready::stops`): what `--dry-run` says would be refused.
    /// Build results are not among them: the pre-pass does not build, and the
    /// ready check judges the branch's build once it has one.
    pub(super) refusals: Vec<String>,
    /// The versions the tool's recap fill covers, `origin..branch` (`1.7.5..1.9.0`),
    /// when the pre-pass names a missing recap and both versions are read.
    pub(super) recap: Option<String>,
}

impl PreGate {
    /// Whether the tool has a fix to make.
    pub(super) fn has_tool(&self) -> bool {
        !self.tool.is_empty()
    }

    /// The names of the tool's findings (`changes.dependency-bullet`, `FLAG`).
    pub(super) fn tool_names(&self) -> Vec<String> {
        self.tool
            .iter()
            .map(|l| {
                l.split_once(": ")
                    .map_or(l.as_str(), |(name, _)| name)
                    .to_string()
            })
            .collect()
    }

    /// The pre-pass findings as the report lists them: the tool's and the agent's,
    /// and each stop that is not already listed as an agent finding.
    pub(super) fn report_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.tool.iter().chain(&self.agent).cloned().collect();
        lines.extend(
            self.refusals
                .iter()
                .filter(|id| !self.agent.iter().any(|l| l.starts_with(&format!("{id}: "))))
                .cloned(),
        );
        lines
    }
}

/// Whether `pkg` carries unpublished work, and what it is compared with.
/// `ecosystem` is the package's source (`None` when it is not one auto-package
/// knows); `continues` is whether a branch's version is decided from the branch
/// (see [`existing_lint::applies`]).
pub(super) fn carry(
    source: &str,
    branch: &str,
    pkg: &str,
    in_place: bool,
    continues: bool,
    ecosystem: Option<&Ecosystem>,
    emit: Emit<'_>,
) -> Option<Carry> {
    if in_place {
        let origin = osc::link_origin(source, pkg, emit).ok().flatten()?;
        let ours = srcmd5(source, pkg, emit)?;
        let theirs = srcmd5(&origin.project, &origin.package, emit)?;
        return (ours != theirs).then(|| Carry {
            work: source.to_string(),
            origin: origin.project,
            origin_pkg: origin.package,
        });
    }
    if !continues {
        return None;
    }
    let decision = decide_versions(
        scheme_of_ecosystem(ecosystem?),
        &|| origin_version(source, pkg, emit),
        &|| branch_version(branch, pkg, emit),
    );
    if !matches!(decision, Continuation::Branch { .. }) {
        return None;
    }
    let (origin, origin_pkg) = match osc::link_origin(branch, pkg, emit) {
        Ok(Some(link)) => (link.project, link.package),
        _ => (source.to_string(), pkg.to_string()),
    };
    Some(Carry {
        work: branch.to_string(),
        origin,
        origin_pkg,
    })
}

/// The revision (expanded srcmd5) of `pkg` in `project`.
pub(super) fn srcmd5(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    osc::package_entries_at_revision(project, pkg, None, emit)
        .ok()??
        .srcmd5
}

/// The pre-pass of a carrying package: the review of its work against the
/// origin, without downloads and without a plan, and the stale FLAG lines.
/// `ldconfig` is whether the target defines `%ldconfig_scriptlets`.
pub(super) fn pre_gate(
    obs: &LiveRequestObs<'_>,
    carry: &Carry,
    pkg: &str,
    ldconfig: bool,
    emit: Emit<'_>,
) -> Result<PreGate, String> {
    let rev = srcmd5(&carry.work, pkg, emit)
        .ok_or_else(|| format!("{} has no revision to review", carry.work))?;
    // Nothing is downloaded, so no scratch directory is written.
    let options = Options {
        id: "",
        download: false,
        scratch: Path::new(""),
        fail_on: Severity::Warning,
        fail_on_agent: false,
        build: env!("AP_BUILD_REVISION"),
    };
    let ldconfig_of = |_: &str| ldconfig;
    let deps = Deps {
        plan: &not_in_pre_pass,
        repology: &no_repology,
        ldconfig_macro: &ldconfig_of,
        host_allowed: &|_: &str| false,
        forge: &NO_FORGE,
    };
    let report = review_branch(
        obs,
        &carry.work,
        pkg,
        &rev,
        &carry.origin,
        &carry.origin_pkg,
        &options,
        &deps,
    )
    .map_err(|e| e.to_string())?;
    let mut gate = classify(report.check_findings());
    if osc::has_explained_flags(&carry.work, pkg, &carry.origin, &carry.origin_pkg, emit) {
        gate.tool.push(
            "FLAG: an unpublished entry's FLAG line is one the origin's spec already explains"
                .to_string(),
        );
    }
    if gate
        .tool
        .iter()
        .any(|l| l.starts_with(ids::CHANGES_NO_UPSTREAM_NOTES))
    {
        gate.recap = recap_range(carry, pkg, emit);
    }
    Ok(gate)
}

/// The versions a recap covers: the origin's `Version:` and the branch's, as
/// `origin..branch`. `None` when either is not read.
fn recap_range(carry: &Carry, pkg: &str, emit: Emit<'_>) -> Option<String> {
    let old = origin_version(&carry.origin, &carry.origin_pkg, emit).ok()?;
    let new = branch_version(&carry.work, pkg, emit).ok()??;
    Some(format!("{old}..{new}"))
}

/// Splits review findings by who fixes them: the tool, the agent, or neither
/// (a maintainer, the pipeline, a note), which the pre-pass leaves alone.
pub(super) fn classify<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> PreGate {
    let mut gate = PreGate::default();
    for f in findings {
        let line = format!("{}: {}", f.id, f.message);
        let routed = route(&ReviewFinding::from(f));
        match routed {
            FindingRoute::Tool(_) => gate.tool.push(line),
            FindingRoute::Agent => gate.agent.push(line),
            _ => {}
        }
        if !matches!(routed, FindingRoute::Tool(_)) && ready::stops(f) && !is_build_state(f) {
            gate.refusals.push(f.id.to_string());
        }
    }
    gate
}

/// Whether a finding is the state of the branch's build (`build.*`).
fn is_build_state(f: &Finding) -> bool {
    f.id.starts_with("build.")
}

fn not_in_pre_pass(_: &ActionInputs) -> Result<Candidate, String> {
    Err(NOT_IN_PRE_PASS.to_string())
}

fn no_repology(_: &str) -> Result<String, String> {
    Err(NOT_IN_PRE_PASS.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_build::request_review::ids;

    fn finding(id: &'static str, severity: Severity) -> Finding {
        Finding::check(id, severity, "pkg", "a message")
    }

    #[test]
    fn a_tool_finding_is_sent_to_the_tool_and_an_agent_one_to_the_agent() {
        let found = [
            finding(ids::CHANGES_DEPENDENCY_BULLET, Severity::Warning),
            finding(ids::FILES_LEFTOVER, Severity::Warning),
            finding(ids::SPEC_STYLE, Severity::Warning),
            finding(ids::FILES_UNREFERENCED, Severity::Warning),
        ];
        let gate = classify(found.iter());
        assert_eq!(gate.tool.len(), 2);
        assert!(gate.tool[0].starts_with("changes.dependency-bullet: "));
        assert_eq!(gate.agent.len(), 2);
        assert!(gate.agent[0].starts_with("spec.style: "));
        assert!(gate.has_tool());
    }

    #[test]
    fn a_maintainer_finding_is_refused_and_a_note_is_not() {
        let found = [
            finding(ids::REQUEST_COMPETING, Severity::Warning),
            finding(ids::VERSION_NOT_CHECKED, Severity::Note),
        ];
        let gate = classify(found.iter());
        assert!(gate.tool.is_empty() && gate.agent.is_empty());
        assert_eq!(gate.refusals, [ids::REQUEST_COMPETING]);
    }

    #[test]
    fn a_build_state_is_not_refused_before_the_build() {
        // The branch's build results are judged after its build, not in the pre-pass.
        let found = [
            finding(ids::BUILD_PENDING, Severity::Note),
            finding(ids::BUILD_FAILS_IN_TARGET_TOO, Severity::Blocker),
        ];
        assert!(classify(found.iter()).refusals.is_empty());
    }

    #[test]
    fn a_tool_fix_is_not_a_refusal_and_an_agent_finding_is() {
        let found = [
            finding(ids::FILES_LEFTOVER, Severity::Warning),
            finding(ids::SPEC_STYLE, Severity::Warning),
        ];
        assert_eq!(classify(found.iter()).refusals, [ids::SPEC_STYLE]);
    }

    #[test]
    fn a_missing_recap_is_a_tool_fix_and_not_a_refusal() {
        let found = [finding(ids::CHANGES_NO_UPSTREAM_NOTES, Severity::Warning)];
        let gate = classify(found.iter());
        assert_eq!(gate.tool_names(), [ids::CHANGES_NO_UPSTREAM_NOTES]);
        assert!(gate.agent.is_empty() && gate.refusals.is_empty());
    }

    #[test]
    fn a_clean_branch_has_nothing_to_send() {
        let none: [Finding; 0] = [];
        assert!(!classify(none.iter()).has_tool());
    }

    #[test]
    fn the_report_lists_each_pre_pass_finding_once() {
        let gate = PreGate {
            tool: vec!["changes.dependency-bullet: x".to_string()],
            agent: vec!["spec.style: y".to_string()],
            refusals: vec!["spec.style".to_string(), "request.competing".to_string()],
            ..Default::default()
        };
        assert_eq!(
            gate.report_lines(),
            [
                "changes.dependency-bullet: x",
                "spec.style: y",
                "request.competing"
            ]
        );
        assert_eq!(gate.tool_names(), ["changes.dependency-bullet"]);
    }
}
