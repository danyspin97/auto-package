//! The ready gate of `lint`: once a carrying package is built, its branch is
//! reviewed against its origin once more, with downloads and without a model
//! call, and the verdict is kept in the package's state directory
//! (`state/<pkg>/ready.json`) with the source srcmd5 it judged. A verdict whose
//! srcmd5 is still the branch's is reused. The submit check reads the verdict
//! later; nothing here refuses a submit.

use super::existing_lint;
use super::leftovers::{self, Carry};
use super::*;
use ap_build::bugfix::{FindingRoute, ReviewFinding, not_submittable, route};
use ap_build::request_review::fetch::{LiveRequestObs, RequestObs};
use ap_build::request_review::ids;
use ap_build::request_review::request::competing_findings;
use ap_build::request_review::run::{Deps, Options};
use ap_build::request_review::synthetic::review_branch;
use ap_build::request_review::upstream_check::{live_plan, live_repology};
use ap_build::request_review::{Finding, Severity};
use ap_build::workspace::Workspace;
use serde::{Deserialize, Serialize};

/// The file of a package's state directory that keeps its verdict.
const VERDICT_FILE: &str = "ready.json";

/// The maintainer's stops: a request to the target is open, the target fails
/// to build too, or a carried patch's upstream was closed. They stop a package
/// whatever their severity.
const MAINTAINER_STOPS: [&str; 3] = [
    ids::REQUEST_COMPETING,
    ids::PATCH_UPSTREAM_CLOSED,
    ids::BUILD_FAILS_IN_TARGET_TOO,
];

/// Whether a finding keeps its package from being ready: a warning or a
/// blocker, any build result that is not a pass, a version other than the
/// update's, or one of the maintainer's stops.
pub(super) fn stops(f: &Finding) -> bool {
    f.severity >= Severity::Warning
        || f.id.starts_with("build.")
        || f.id == ids::CHANGES_VERSION_MISMATCH
        || MAINTAINER_STOPS.contains(&f.id)
}

/// The verdict of the ready gate on one package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Verdict {
    /// The source revision the verdict judged.
    pub(super) srcmd5: String,
    /// The ids that stop the package: empty when it is ready.
    pub(super) reasons: Vec<String>,
    /// Whether a reason is a finding the agent fixes (with `--no-agent` it is
    /// not fixed, so the package is not ready).
    pub(super) needs_agent: bool,
}

impl Verdict {
    pub(super) fn ready(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// The verdict on `srcmd5` from the findings of its review and the other open
/// requests to the target (`competing`). `agent` is whether an agent runs: its
/// findings stop a package only when none does.
pub(super) fn judge<'a>(
    srcmd5: &str,
    findings: impl IntoIterator<Item = &'a Finding>,
    agent: bool,
) -> Verdict {
    let mut reasons: Vec<String> = Vec::new();
    let mut needs_agent = false;
    let mut owned: Vec<ReviewFinding> = Vec::new();
    for f in findings {
        let rf = ReviewFinding::from(f);
        let agent_route = matches!(route(&rf), FindingRoute::Agent);
        owned.push(rf);
        let agent_stop = agent_route && !agent && f.severity != Severity::Note;
        if stops(f) || agent_stop {
            if !reasons.iter().any(|r| r == f.id) {
                reasons.push(f.id.to_string());
            }
            needs_agent |= agent_route;
        }
    }
    // A refusal of the submit rule the findings above did not already name.
    if let Some(refusal) = not_submittable(&owned)
        && reasons.is_empty()
    {
        reasons.push(refusal.message().to_string());
    }
    Verdict {
        srcmd5: srcmd5.to_string(),
        reasons,
        needs_agent,
    }
}

/// The ready gate of the packages `results` built in this run that carry
/// unpublished work. Prints a summary line per package.
pub(super) fn check_built(
    a: &UpdateProjectArgs,
    branch: &str,
    results: &[ItemResult],
    ctx: &Ctx<'_>,
) {
    let emit = Emit::run(ctx.reporter.as_ref());
    let continues = existing_lint::applies(a);
    for result in results {
        if !matches!(result.outcome, ItemOutcome::Updated) {
            continue;
        }
        let item = &result.item;
        let Some(carry) = leftovers::carry(
            &a.source,
            branch,
            &item.pkg,
            a.in_place,
            continues,
            Some(&item.ecosystem),
            emit,
        ) else {
            continue;
        };
        match verdict_of(a, &carry, &item.pkg, ctx) {
            Ok((verdict, reused)) => {
                let kept = if reused {
                    " (kept from an earlier check of the same revision)"
                } else {
                    ""
                };
                if verdict.ready() {
                    ctx.say(format!("{}: ready to submit{kept}", item.pkg));
                } else {
                    let needs = if verdict.needs_agent {
                        " (needs the agent)"
                    } else {
                        ""
                    };
                    ctx.say(format!(
                        "{}: not ready{needs}: {}{kept}",
                        item.pkg,
                        verdict.reasons.join(", ")
                    ));
                }
            }
            Err(why) => ctx.say(format!(
                "{}: not ready: the ready check could not run: {why}",
                item.pkg
            )),
        }
    }
}

/// The verdict on the branch of `pkg`, kept when it judged the same revision.
/// The flag is whether it was kept.
fn verdict_of(
    a: &UpdateProjectArgs,
    carry: &Carry,
    pkg: &str,
    ctx: &Ctx<'_>,
) -> Result<(Verdict, bool), String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let srcmd5 = leftovers::srcmd5(&carry.work, pkg, emit)
        .ok_or_else(|| format!("{} has no revision", carry.work))?;
    let state =
        Workspace::find_by_obs_name(&ctx.state_root, pkg).map(|ws| ws.dir().join(VERDICT_FILE));
    if let Some(kept) = state.as_deref().and_then(read_verdict)
        && kept.srcmd5 == srcmd5
    {
        return Ok((kept, true));
    }
    let verdict = review(a, carry, pkg, &srcmd5, ctx)?;
    match &state {
        Some(path) => {
            if let Err(e) = write_verdict(path, &verdict) {
                ctx.warn(format!("{pkg}: the verdict could not be kept: {e}"));
            }
        }
        None => ctx.warn(format!("{pkg}: no build state to keep the verdict in")),
    }
    Ok((verdict, false))
}

/// The full review of the branch at `srcmd5`, against the origin, with downloads
/// and the live lookups (the same as `review`), and the open requests to it.
fn review(
    a: &UpdateProjectArgs,
    carry: &Carry,
    pkg: &str,
    srcmd5: &str,
    ctx: &Ctx<'_>,
) -> Result<Verdict, String> {
    let emit = Emit::run(ctx.reporter.as_ref());
    let obs = LiveRequestObs { emit };
    let hosts = ap_build::allowed_hosts::load_default();
    let host_allowed = |host: &str| hosts.allows(host, ap_build::allowed_hosts::Use::Source);
    let forge = ap_build::patch_provenance::LiveForge {
        package: "ready",
        hosts: &hosts,
    };
    let plan = live_plan(emit);
    let deps = Deps {
        plan: &plan,
        repology: &live_repology,
        ldconfig_macro: &crate::review_cmd::ldconfig_macro,
        host_allowed: &host_allowed,
        forge: &forge,
    };
    let scratch = ctx.work_dir.join("ready").join(pkg);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let options = Options {
        id: "",
        download: true,
        scratch: &scratch,
        fail_on: Severity::Warning,
        fail_on_agent: false,
        build: env!("AP_BUILD_REVISION"),
    };
    let reviewed = review_branch(
        &obs,
        &carry.work,
        pkg,
        srcmd5,
        &carry.origin,
        &carry.origin_pkg,
        &options,
        &deps,
    );
    let _ = std::fs::remove_dir_all(&scratch);
    let report = reviewed.map_err(|e| e.to_string())?;
    let open = obs
        .open_requests_to(&carry.origin, &carry.origin_pkg)
        .map_err(|e| {
            format!(
                "the open requests to {} could not be read: {}",
                carry.origin, e.what
            )
        })?;
    let competing = competing_findings(&open, "", &carry.origin_pkg);
    Ok(judge(
        srcmd5,
        report.check_findings().chain(&competing),
        !a.no_agent,
    ))
}

fn read_verdict(path: &Path) -> Option<Verdict> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_verdict(path: &Path, verdict: &Verdict) -> Result<(), String> {
    let text = serde_json::to_string_pretty(verdict).map_err(|e| e.to_string())?;
    ap_core::fsx::write_atomic(path, text.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &'static str, severity: Severity) -> Finding {
        Finding::check(id, severity, "pkg", "a message")
    }

    #[test]
    fn a_warning_or_a_build_that_is_not_a_pass_stops_a_package() {
        assert!(stops(&finding(ids::SPEC_STYLE, Severity::Warning)));
        assert!(stops(&finding(ids::SPEC_STYLE, Severity::Blocker)));
        assert!(!stops(&finding(ids::SPEC_STYLE, Severity::Note)));
        assert!(stops(&finding(ids::BUILD_PENDING, Severity::Note)));
        assert!(stops(&finding(
            ids::CHANGES_VERSION_MISMATCH,
            Severity::Note
        )));
        assert!(stops(&finding(ids::REQUEST_COMPETING, Severity::Note)));
        assert!(stops(&finding(ids::PATCH_UPSTREAM_CLOSED, Severity::Note)));
    }

    #[test]
    fn a_clean_review_with_no_competing_request_is_ready() {
        let found = [finding(ids::VERSION_NOT_CHECKED, Severity::Note)];
        let verdict = judge("abc", found.iter(), true);
        assert!(verdict.ready());
        assert_eq!(verdict.srcmd5, "abc");
        assert!(!verdict.needs_agent);
    }

    #[test]
    fn a_competing_request_and_a_build_failure_each_stop_the_package() {
        let found = [
            finding(ids::REQUEST_COMPETING, Severity::Warning),
            finding(ids::BUILD_FAILED, Severity::Blocker),
        ];
        let verdict = judge("abc", found.iter(), true);
        assert!(!verdict.ready());
        assert_eq!(verdict.reasons, [ids::REQUEST_COMPETING, ids::BUILD_FAILED]);
    }

    #[test]
    fn with_no_agent_an_agent_finding_is_not_ready_and_needs_the_agent() {
        let found = [finding(ids::SPEC_STYLE, Severity::Warning)];
        let verdict = judge("abc", found.iter(), false);
        assert!(!verdict.ready());
        assert!(verdict.needs_agent);
        assert_eq!(verdict.reasons, [ids::SPEC_STYLE]);
    }

    #[test]
    fn a_verdict_is_kept_as_json_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(VERDICT_FILE);
        let verdict = judge("abc", [].iter(), true);
        write_verdict(&path, &verdict).unwrap();
        assert_eq!(read_verdict(&path), Some(verdict));
        assert_eq!(read_verdict(&dir.path().join("absent.json")), None);
    }
}
