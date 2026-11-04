//! The decisions of `fix --request` that need no OBS: what a saved review
//! says, how its findings group, what a re-check changed, and when the request
//! may be superseded. Nothing here reads or writes anything.

use ap_build::bugfix::{FindingRoute, ReviewFinding, route};
use ap_build::request_review::request::{ActionSource, ActionTarget, RequestDoc};
use ap_build::request_review::{Origin, Severity};
use serde_json::Value;

use super::FixExit;
use super::plan::{EXIT_NEEDS_MAINTAINER, EXIT_NO_FIX};

/// The findings and the revision of a review saved as `review.json`.
#[derive(Debug, PartialEq)]
pub(super) struct Saved {
    pub(super) findings: Vec<ReviewFinding>,
    /// The revision the review read, `None` when the request pins none.
    pub(super) rev: Option<String>,
    /// The source project and package the review was of.
    pub(super) source: (String, String),
}

/// One finding of a saved review from its JSON form.
fn finding_from(f: &Value) -> Result<ReviewFinding, String> {
    let text = |key: &str| {
        f[key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("a finding has no {key}"))
    };
    let severity = match f["severity"].as_str() {
        Some("note") => Severity::Note,
        Some("warning") => Severity::Warning,
        Some("blocker") => Severity::Blocker,
        other => return Err(format!("a finding has the severity {other:?}")),
    };
    let origin = match f["origin"].as_str() {
        Some("check") => Origin::Check,
        Some("agent") => Origin::Agent,
        other => return Err(format!("a finding has the origin {other:?}")),
    };
    Ok(ReviewFinding {
        id: text("id")?,
        severity,
        origin,
        package: f["package"].as_str().unwrap_or_default().to_string(),
        file: f["file"].as_str().map(str::to_string),
        line: f["line"].as_u64().map(|n| n as usize),
        message: f["message"].as_str().unwrap_or_default().to_string(),
    })
}

/// Read a saved `review.json` of request `request_id` (schema 1, one action).
pub(super) fn parse_saved(text: &str, request_id: &str) -> Result<Saved, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not a review.json: {e}"))?;
    if v["schema"].as_u64() != Some(1) {
        return Err("the review is not of schema 1".to_string());
    }
    let id = v["request"]["id"].as_str().unwrap_or_default();
    if id != request_id {
        return Err(format!("the review is of request {id}, not {request_id}"));
    }
    let actions = v["actions"].as_array().map_or(&[][..], Vec::as_slice);
    let [action] = actions else {
        return Err(format!(
            "the review has {} actions: one submit action is needed",
            actions.len()
        ));
    };
    let source = &action["source"];
    let mut findings = Vec::new();
    let all = v["request"]["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(action["findings"].as_array().into_iter().flatten());
    for f in all {
        findings.push(finding_from(f)?);
    }
    Ok(Saved {
        findings,
        rev: source["rev"].as_str().map(str::to_string),
        source: (
            source["project"].as_str().unwrap_or_default().to_string(),
            source["package"].as_str().unwrap_or_default().to_string(),
        ),
    })
}

/// Why a saved review is not of the request's source: another project, another
/// package, or another revision. `None` when it is of the same source.
pub(super) fn saved_mismatch(saved: &Saved, source: &ActionSource) -> Option<String> {
    let (project, package) = &saved.source;
    if *project != source.project || *package != source.package {
        return Some(format!(
            "the review is of {project} {package}, the request names {} {}: review it again",
            source.project, source.package
        ));
    }
    if saved.rev != source.rev {
        return Some(format!(
            "the review is of revision {}, the request now names {}: review it again",
            saved.rev.as_deref().unwrap_or("(none)"),
            source.rev.as_deref().unwrap_or("(none)")
        ));
    }
    None
}

/// Whether a request may be fixed here: exactly one submit action, in a state
/// that is still open, from the user's own branch project to another project
/// of the same package. The refusal carries the exit status.
pub(super) fn request_refusal(
    doc: &RequestDoc,
    user: &str,
) -> Result<(ActionSource, ActionTarget), FixExit> {
    let refuse = |code: i32, message: String| FixExit { code, message };
    let id = &doc.id;
    let [action] = doc.actions.as_slice() else {
        return Err(refuse(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {id} has {} actions: only one submit action can be fixed",
                doc.actions.len()
            ),
        ));
    };
    if action.kind != "submit" {
        return Err(refuse(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {id} is a {} request: only a submit request can be fixed",
                action.kind
            ),
        ));
    }
    match doc.state.name.as_str() {
        "new" | "review" | "declined" => {}
        "accepted" | "revoked" | "superseded" => {
            return Err(refuse(
                EXIT_NO_FIX,
                format!("request {id} is {}: nothing to fix", doc.state.name),
            ));
        }
        other => {
            return Err(refuse(
                EXIT_NEEDS_MAINTAINER,
                format!("request {id} is in the state {other:?}, which is not fixed here"),
            ));
        }
    }
    let (Some(source), Some(target)) = (action.source.clone(), action.target.clone()) else {
        return Err(refuse(
            EXIT_NEEDS_MAINTAINER,
            format!("request {id} does not name a source and a target"),
        ));
    };
    // The source is one of the user's branches: `home:<user>:branches:<project>`.
    let own = !user.is_empty()
        && source
            .project
            .strip_prefix(&format!("home:{user}:branches:"))
            .is_some_and(|project| !project.is_empty())
        && source.project != target.project;
    if !own {
        return Err(refuse(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {id} comes from {}, which is not a branch project of {user}: ask the request's creator",
                source.project
            ),
        ));
    }
    if source.package != target.package {
        return Err(refuse(
            EXIT_NEEDS_MAINTAINER,
            format!(
                "request {id} renames {} to {}: a renamed package is not fixed here",
                source.package, target.package
            ),
        ));
    }
    Ok((source, target))
}

/// Whether a finding is one the run acts on (a tool, the agent, the pipeline
/// or OBS), rather than a person's or nothing.
pub(super) fn is_actionable(route: &FindingRoute) -> bool {
    matches!(
        route,
        FindingRoute::Tool(_) | FindingRoute::Agent | FindingRoute::Pipeline | FindingRoute::Wait
    )
}

/// The same finding in two reviews: same id, file and package.
fn same(a: &ReviewFinding, b: &ReviewFinding) -> bool {
    a.id == b.id && a.file == b.file && a.package == b.package
}

/// What a re-check changed: the findings it no longer makes, the ones that
/// are new, and the ones still there.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Delta {
    pub(super) resolved: Vec<ReviewFinding>,
    pub(super) introduced: Vec<ReviewFinding>,
    pub(super) left: Vec<ReviewFinding>,
}

pub(super) fn delta(before: &[ReviewFinding], after: &[ReviewFinding]) -> Delta {
    let mut out = Delta::default();
    for f in before {
        if after.iter().any(|a| same(a, f)) {
            out.left.push(f.clone());
        } else {
            out.resolved.push(f.clone());
        }
    }
    out.introduced = after
        .iter()
        .filter(|a| !before.iter().any(|b| same(a, b)))
        .cloned()
        .collect();
    out
}

/// Why the request must not be superseded, with the exit status, after a
/// re-check left `after`. `None` when every finding is resolved or is a
/// person's to decide (a warning with a maintainer route).
pub(super) fn not_submittable(after: &[ReviewFinding]) -> Option<FixExit> {
    let mut maintainer_blocker = None;
    let mut unresolved = Vec::new();
    for f in after {
        let r = route(f);
        if f.severity == Severity::Note || matches!(r, FindingRoute::NotActedOn) {
            continue;
        }
        match r {
            FindingRoute::Maintainer(_) if f.severity == Severity::Blocker => {
                maintainer_blocker = Some(f);
            }
            FindingRoute::Maintainer(_) => {}
            _ => unresolved.push(f),
        }
    }
    if let Some(f) = maintainer_blocker {
        return Some(FixExit {
            code: EXIT_NEEDS_MAINTAINER,
            message: format!(
                "a blocker needs a maintainer: {} {}: nothing was submitted; the branch keeps the fixes",
                f.id,
                f.file.as_deref().unwrap_or_default()
            ),
        });
    }
    if !unresolved.is_empty() {
        let ids: Vec<&str> = unresolved.iter().map(|f| f.id.as_str()).collect();
        return Some(FixExit {
            code: 1,
            message: format!(
                "{} finding(s) are not resolved ({}): nothing was submitted; the branch keeps the fixes",
                unresolved.len(),
                ids.join(", ")
            ),
        });
    }
    None
}

/// Whether `--yes` must refuse: a finding a person must decide is among the
/// remaining ones (any risk flag of the review, or a maintainer route).
pub(super) fn needs_person(after: &[ReviewFinding]) -> Option<String> {
    let names: Vec<String> = after
        .iter()
        .filter(|f| f.severity != Severity::Note && matches!(route(f), FindingRoute::Maintainer(_)))
        .map(|f| f.id.clone())
        .collect();
    (!names.is_empty()).then(|| format!("a person must decide: {}", names.join(", ")))
}

/// The ids of some findings, sorted and without repeats; `none` for no finding.
fn id_list(list: &[ReviewFinding]) -> String {
    let mut ids: Vec<&str> = list.iter().map(|f| f.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    }
}

/// The description of the request a findings fix creates: what it supersedes,
/// the findings it fixes and the ones it leaves to a person. Notes are not
/// listed.
pub(super) fn findings_description(
    old: &str,
    resolved: &[ReviewFinding],
    remaining: &[ReviewFinding],
) -> String {
    let remaining: Vec<ReviewFinding> = remaining
        .iter()
        .filter(|f| f.severity != Severity::Note)
        .cloned()
        .collect();
    format!(
        "Supersedes request {old}; fixes: {}; remaining: {}.",
        id_list(resolved),
        id_list(&remaining)
    )
}
