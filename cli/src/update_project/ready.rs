//! The ready gate of `lint`: once a carrying package is built, its branch is
//! reviewed against its origin once more, with downloads and without a model
//! call, and the verdict is kept in the package's state directory
//! (`state/<pkg>/ready.json`) with the source srcmd5 it judged. A verdict whose
//! srcmd5 is still the branch's is reused. The submit check reads the verdict
//! later: `submit_gate` judges a submit with it (see `prepare_request`).

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
    /// One line per stopping finding (`id file: message`), for the refusal of a
    /// submit. Empty in a verdict written before this field existed.
    #[serde(default)]
    pub(super) details: Vec<String>,
}

impl Verdict {
    pub(super) fn ready(&self) -> bool {
        self.reasons.is_empty()
    }

    /// Whether this verdict may be reused for `srcmd5`: it judged that revision,
    /// and, when it is not ready, it kept the details of why.
    pub(super) fn judged(&self, srcmd5: &str) -> bool {
        self.srcmd5 == srcmd5 && (self.ready() || !self.details.is_empty())
    }
}

/// The verdict on a revision: the stored one when it judged that revision
/// (see [`Verdict::judged`]), else `fresh()`. The flag is whether the stored
/// one was kept.
pub(super) fn settled(
    stored: Option<Verdict>,
    srcmd5: &str,
    fresh: impl FnOnce() -> Result<Verdict, String>,
) -> Result<(Verdict, bool), String> {
    match stored {
        Some(kept) if kept.judged(srcmd5) => Ok((kept, true)),
        _ => fresh().map(|verdict| (verdict, false)),
    }
}

/// When a ready check runs for a submit (see `prepare_request`): never for
/// `update`, `build` and `fix`; for `lint`, with its agent setting.
#[derive(Clone, Copy)]
pub(crate) enum ReadyCheck {
    Skip,
    Gate { agent: bool },
}

/// The check a submit of this run gets.
pub(super) fn check_for(a: &UpdateProjectArgs) -> ReadyCheck {
    if a.improve {
        ReadyCheck::Gate { agent: !a.no_agent }
    } else {
        ReadyCheck::Skip
    }
}

/// What the gate says of a submit.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GateOutcome {
    Ready,
    /// The refusal the request is recorded with.
    NotReady(String),
    /// The gate could not judge: nothing is requested.
    Failed(String),
}

/// The refusal of a package that is not ready: its stopping findings.
pub(super) fn refusal(verdict: &Verdict) -> String {
    format!("not ready to submit: {}", verdict.details.join("; "))
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
    let mut details: Vec<String> = Vec::new();
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
            details.push(format!(
                "{} {}: {}",
                f.id,
                f.file.as_deref().unwrap_or("-"),
                f.message
            ));
            needs_agent |= agent_route;
        }
    }
    // A refusal of the submit rule the findings above did not already name.
    if let Some(refusal) = not_submittable(&owned)
        && reasons.is_empty()
    {
        reasons.push(refusal.message().to_string());
        details.push(refusal.message().to_string());
    }
    Verdict {
        srcmd5: srcmd5.to_string(),
        reasons,
        needs_agent,
        details,
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
    settle(
        &ctx.state_root,
        &ctx.work_dir.join("ready"),
        carry,
        pkg,
        &srcmd5,
        !a.no_agent,
        ctx.reporter.as_ref(),
    )
}

/// The verdict on `pkg` at `srcmd5`, kept in its state directory (see
/// [`settled`]). `scratch_root` holds the review's downloads.
pub(super) fn settle(
    state_root: &Path,
    scratch_root: &Path,
    carry: &Carry,
    pkg: &str,
    srcmd5: &str,
    agent: bool,
    reporter: &(dyn Reporter + Send + Sync),
) -> Result<(Verdict, bool), String> {
    let state = Workspace::find_by_obs_name(state_root, pkg).map(|ws| ws.dir().join(VERDICT_FILE));
    let stored = state.as_deref().and_then(read_verdict);
    let (verdict, kept) = settled(stored, srcmd5, || {
        review(carry, pkg, srcmd5, agent, scratch_root, reporter)
    })?;
    if !kept {
        let warn = |message: String| {
            reporter.report(&Event::Warning {
                step: None,
                message,
            })
        };
        match &state {
            Some(path) => {
                if let Err(e) = write_verdict(path, &verdict) {
                    warn(format!("{pkg}: the verdict could not be kept: {e}"));
                }
            }
            None => warn(format!("{pkg}: no build state to keep the verdict in")),
        }
    }
    Ok((verdict, kept))
}

/// The ready gate of a submit of `item` at its branch revision `srcmd5`: the
/// verdict, judged or kept, decides whether the request may be made.
pub(super) fn submit_gate(
    state_root: &Path,
    branch: &str,
    item: &Item,
    srcmd5: &str,
    agent: bool,
    reporter: &(dyn Reporter + Send + Sync),
) -> GateOutcome {
    // The submit's own origin: the request targets it with the same package name.
    let carry = Carry {
        work: branch.to_string(),
        origin: item.origin.clone(),
        origin_pkg: item.pkg.clone(),
    };
    let scratch = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => return GateOutcome::Failed(format!("no scratch directory: {e}")),
    };
    match settle(
        state_root,
        scratch.path(),
        &carry,
        &item.pkg,
        srcmd5,
        agent,
        reporter,
    ) {
        Ok((verdict, _)) if verdict.ready() => GateOutcome::Ready,
        Ok((verdict, _)) => GateOutcome::NotReady(refusal(&verdict)),
        Err(why) => GateOutcome::Failed(format!("the ready check could not run: {why}")),
    }
}

/// The full review of the branch at `srcmd5`, against the origin, with downloads
/// and the live lookups (the same as `review`), and the open requests to it.
fn review(
    carry: &Carry,
    pkg: &str,
    srcmd5: &str,
    agent: bool,
    scratch_root: &Path,
    reporter: &(dyn Reporter + Send + Sync),
) -> Result<Verdict, String> {
    let emit = Emit::run(reporter);
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
    let scratch = scratch_root.join(pkg);
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
        agent,
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

    #[test]
    fn a_refusal_names_each_stopping_finding_with_its_file() {
        let mut spec = finding(ids::SPEC_STYLE, Severity::Warning);
        spec.file = Some("python-foo.spec".into());
        let found = [finding(ids::REQUEST_COMPETING, Severity::Warning), spec];
        let verdict = judge("abc", found.iter(), true);
        assert_eq!(
            refusal(&verdict),
            "not ready to submit: request.competing -: a message; \
             spec.style python-foo.spec: a message"
        );
    }

    #[test]
    fn a_maintainer_stop_refuses_a_submit_whatever_its_severity() {
        let found = [finding(ids::PATCH_UPSTREAM_CLOSED, Severity::Note)];
        let verdict = judge("abc", found.iter(), true);
        assert!(!verdict.ready());
        assert!(refusal(&verdict).contains(ids::PATCH_UPSTREAM_CLOSED));
    }

    #[test]
    fn a_stored_verdict_of_the_same_revision_is_reused_without_a_review() {
        let stored = judge("abc", [].iter(), true);
        let mut reviewed = false;
        let (kept, was_kept) = settled(Some(stored.clone()), "abc", || {
            reviewed = true;
            Err("no review".to_string())
        })
        .unwrap();
        assert!(was_kept && !reviewed);
        assert_eq!(kept, stored);
    }

    #[test]
    fn a_verdict_of_another_revision_or_without_details_is_judged_again() {
        let fresh = || Ok(judge("new", [].iter(), true));
        let (v, kept) = settled(Some(judge("abc", [].iter(), true)), "new", fresh).unwrap();
        assert!(!kept && v.srcmd5 == "new");
        let old = Verdict {
            srcmd5: "abc".into(),
            reasons: vec![ids::SPEC_STYLE.into()],
            needs_agent: true,
            details: Vec::new(),
        };
        assert!(!old.judged("abc"));
        let (_, kept) = settled(Some(old), "abc", fresh).unwrap();
        assert!(!kept);
        let (_, kept) = settled(None, "abc", fresh).unwrap();
        assert!(!kept);
    }
}
