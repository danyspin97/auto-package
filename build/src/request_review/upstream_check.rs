//! The packaged version against upstream, decided by the update planner the
//! `update --revision` command uses, with Repology as a second opinion.

use super::gather::{ActionInputs, Skipped};
use super::{Finding, Severity, ids};
use crate::proc::Emit;
use crate::update::{Candidate, Decision, Ecosystem, is_newer};

/// The findings of a planner answer. `packaged` is the request's version.
/// `repology` is Repology's newest version, when it was asked.
pub fn version_findings(
    package: &str,
    packaged: &str,
    candidate: &Candidate,
    repology: Option<Result<String, String>>,
) -> (Vec<Finding>, Vec<Skipped>) {
    let mk = |id, sev, msg: &str| Finding::check(id, sev, package, msg);
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    // What the primary lookup says is newest, to hold Repology against.
    let mut primary_newest = packaged.to_string();
    match &candidate.decision {
        Decision::Update { new_version, .. } => {
            primary_newest = new_version.clone();
            out.push(mk(
                ids::VERSION_BEHIND_UPSTREAM,
                Severity::Warning,
                &format!("upstream has {new_version}; this request packages {packaged}"),
            ));
        }
        Decision::UpToDate { .. } => {}
        Decision::Skipped(reason) => out.push(mk(
            ids::VERSION_NOT_CHECKED,
            Severity::Note,
            &format!("the version was not compared with upstream: {reason}"),
        )),
    }
    match repology {
        Some(Ok(latest)) if is_newer(&latest, packaged) && is_newer(&latest, &primary_newest) => {
            out.push(mk(
                ids::VERSION_REPOLOGY_NEWER,
                Severity::Note,
                &format!(
                    "Repology lists {latest}, newer than the packaged {packaged} (Repology can be wrong)"
                ),
            ));
        }
        Some(Err(why)) => skipped.push(Skipped {
            check: "Repology".into(),
            why,
        }),
        _ => {}
    }
    (out, skipped)
}

/// The host of a URL, lower case, without userinfo and port.
fn host_of(url: &str) -> Option<String> {
    let authority = url.split_once("://")?.1.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The hosts the lookups of this spec would contact besides the standard
/// registries (PyPI, rubygems, MetaCPAN, Repology): the forge or index its
/// `Source` and `URL` name, and the repository its `_service` names. A spec
/// from a request is not trusted to choose them.
fn hosts_named(a: &ActionInputs, header: &crate::spec::Header) -> Vec<String> {
    let mut urls: Vec<String> = crate::update::ecosystems(header, None)
        .into_iter()
        .filter_map(|e| match e {
            Ecosystem::GitRepo { url } | Ecosystem::Index { url } => Some(url),
            _ => None,
        })
        .collect();
    if let Some(service) = a.new.service() {
        for part in service.split("<param name=\"url\">").skip(1) {
            if let Some((url, _)) = part.split_once("</param>") {
                urls.push(url.trim().to_string());
            }
        }
    }
    let mut hosts: Vec<String> = urls.iter().filter_map(|u| host_of(u)).collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Plan the package, then ask Repology when the planner's own source was not
/// Repology and the spec names a project there. `plan` is the planner, as
/// `branch_revision::plan_one` runs it; `repology` the lookup of a project.
/// `allowed` says whether a host may be contacted (the allowed-hosts list): the
/// planner is not run when the spec names a host that is not.
pub fn upstream_report(
    a: &ActionInputs,
    plan: &dyn Fn(&ActionInputs) -> Result<Candidate, String>,
    repology: &dyn Fn(&str) -> Result<String, String>,
    allowed: &dyn Fn(&str) -> bool,
) -> (Vec<Finding>, Vec<Skipped>) {
    let package = a.target.package.as_str();
    let Some(header) = a.new.spec().and_then(crate::spec::parse_header) else {
        return (Vec::new(), Vec::new());
    };
    if let Some(host) = hosts_named(a, &header).into_iter().find(|h| !allowed(h)) {
        return (
            vec![Finding::check(
                ids::VERSION_NOT_CHECKED,
                Severity::Note,
                package,
                &format!(
                    "the version was not compared with upstream: the spec names the host {host}, \
                     which the allowed-hosts list does not allow (`hosts allow` adds it)"
                ),
            )],
            Vec::new(),
        );
    }
    let candidate = match plan(a) {
        Ok(c) => c,
        Err(e) => {
            return (
                vec![Finding::check(
                    ids::REVIEW_INCOMPLETE,
                    Severity::Warning,
                    package,
                    &format!("the version could not be compared with upstream: {e}"),
                )],
                Vec::new(),
            );
        }
    };
    let decided_by = match &candidate.decision {
        Decision::Update { ecosystem, .. } | Decision::UpToDate { ecosystem, .. } => {
            Some(ecosystem)
        }
        Decision::Skipped(_) => None,
    };
    let project = (!matches!(decided_by, Some(Ecosystem::Repology { .. })))
        .then(|| {
            crate::update::ecosystems(&header, None)
                .into_iter()
                .find_map(|e| match e {
                    Ecosystem::Repology { project } => Some(project),
                    _ => None,
                })
        })
        .flatten();
    let second = project.map(|p| repology(&p));
    version_findings(package, &header.version, &candidate, second)
}

/// The live planner: the spec at the action's revision, classified with the
/// lookups the update sweep uses.
pub fn live_plan(emit: Emit<'_>) -> impl Fn(&ActionInputs) -> Result<Candidate, String> + '_ {
    move |a| {
        let pkg = a.source.package.clone();
        crate::branch_revision::plan_one(
            &a.source.project,
            &|n| n == pkg,
            a.source.rev.as_deref(),
            None,
            emit,
        )
    }
}

/// The live Repology lookup, kept in the upstream cache.
pub fn live_repology(project: &str) -> Result<String, String> {
    crate::upstream_cache::lookup(&format!("repology:{project}"), || {
        crate::upstream::latest_on_repology(project)
    })
}
