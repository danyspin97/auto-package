//! Packages an open submit request already updates. Before a package is
//! branched, the open submit requests (new or in review) to its origin are
//! looked at: one that brings the planned version, or a newer one, leaves the
//! package out, so the work is not done twice. A request from anyone counts,
//! the user's own branch project included. A request that brings an older
//! version is only noted. The check is made for the upgrade of a package in a
//! plain `update`, and `--include-requested` turns it off. A read that fails
//! never leaves a package out.
//!
//! The requests to an origin are read once per run, for the whole project, and
//! the spec of a request's source is read only for a request to the package
//! at hand.

use std::cmp::Ordering;
use std::collections::HashMap;

use super::*;
use ap_build::version::{self, Scheme};

/// What the open requests say about one item.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Verdict {
    /// The request that makes the item a duplicate, if any.
    pub(super) skip: Option<update::AlreadyRequested>,
    /// Requests that do not, said in the plan.
    pub(super) notes: Vec<String>,
}

/// Whether the check is made for an item at all: the upgrade of a package in a
/// plain `update`, not `--include-requested`, not a maintenance run (it has its
/// own request handling), not a bug fix.
pub(super) fn applies(item: &Item, a: &UpdateProjectArgs) -> bool {
    existing::applies(item, a)
        && !a.include_requested
        && a.maintenance.is_none()
        && !a.maintenance_list
        && !a.maintenance_source
        && a.fix_task.is_none()
}

fn scheme_of(item: &Item) -> Scheme {
    match item.ecosystem {
        Ecosystem::Pypi { .. } => Scheme::Pep440,
        _ => Scheme::Generic,
    }
}

/// Decide from `requests`, the open submit requests to the item's origin (all
/// its packages), or the reason they could not be read. `version_of` reads the
/// `Version:` a request brings.
pub(super) fn decide(
    item: &Item,
    a: &UpdateProjectArgs,
    requests: Result<&[osc::OpenRequest], &str>,
    version_of: &dyn Fn(&osc::OpenRequest) -> Result<String, String>,
) -> Verdict {
    let mut verdict = Verdict::default();
    let Ok(requests) = requests else {
        return verdict;
    };
    if !applies(item, a) {
        return verdict;
    }
    let scheme = scheme_of(item);
    for r in requests {
        if !matches!(r.state.as_str(), "new" | "review")
            || r.target_package != item.pkg
            || r.target_project != item.origin
        {
            continue;
        }
        let brings = match version_of(r) {
            Ok(v) => v,
            Err(why) => {
                verdict.notes.push(format!(
                    "request {} may bring an update, but its version could not be read: {why}",
                    r.id
                ));
                continue;
            }
        };
        // A snapshot (a commit, a nightly build) does not bring a release: it is
        // noted, however new its number is.
        if version::is_snapshot(&brings) && !version::is_snapshot(&item.new_version) {
            verdict.notes.push(format!(
                "request {} brings the snapshot {brings}, not the release {}",
                r.id, item.new_version
            ));
            continue;
        }
        let order = if brings.trim() == item.new_version.trim() {
            Some(Ordering::Equal)
        } else {
            version::cmp(&brings, &item.new_version, scheme)
        };
        match order {
            Some(Ordering::Less) => verdict.notes.push(format!(
                "request {} brings {brings}, newer target {}",
                r.id, item.new_version
            )),
            Some(_) => {
                verdict.skip = Some(update::AlreadyRequested {
                    id: r.id.clone(),
                    package: item.pkg.clone(),
                    version: brings,
                    creator: r.creator.clone(),
                    state: r.state.clone(),
                });
                return verdict;
            }
            None => verdict.notes.push(format!(
                "request {} brings {brings}, which cannot be compared with {}",
                r.id, item.new_version
            )),
        }
    }
    verdict
}

/// The open requests to each origin read so far, grouped by target package. An
/// origin is read once; a read that failed is warned about once and not tried
/// again, and then says nothing.
#[derive(Default)]
pub(super) struct OpenRequests {
    by_origin: HashMap<String, Option<HashMap<String, Vec<osc::OpenRequest>>>>,
}

impl OpenRequests {
    /// The open requests to `origin/pkg`, or `None` when the origin could not
    /// be read. `read` reads the requests to a project.
    pub(super) fn to_package(
        &mut self,
        origin: &str,
        pkg: &str,
        read: &dyn Fn(&str) -> Result<Vec<osc::OpenRequest>, String>,
        warn: &dyn Fn(String),
    ) -> Option<&[osc::OpenRequest]> {
        self.by_origin
            .entry(origin.to_string())
            .or_insert_with(|| match read(origin) {
                Ok(all) => {
                    let mut grouped: HashMap<String, Vec<osc::OpenRequest>> = HashMap::new();
                    for r in all {
                        grouped.entry(r.target_package.clone()).or_default().push(r);
                    }
                    Some(grouped)
                }
                Err(why) => {
                    warn(format!(
                        "could not read the open requests to {origin}, so a package that is \
                         already requested is not detected: {why}"
                    ));
                    None
                }
            })
            .as_ref()
            .map(|g| g.get(pkg).map_or(&[][..], Vec::as_slice))
    }
}

/// The most of a spec that its version is read from. A spec is a few kilobytes;
/// a larger one is not taken for the spec. (The read itself is not capped.)
const SPEC_CAP: usize = 2 * 1024 * 1024;

/// The `Version:` the source of `r` has at the revision the request carries. The
/// spec is the one the source's listing holds, as the convention picks it.
fn source_version(r: &osc::OpenRequest, emit: Emit<'_>) -> Result<String, String> {
    let rev = r.source_rev.as_deref();
    let listing = osc::package_entries_at_revision(&r.source_project, &r.source_package, rev, emit)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "its source has no package at that revision".to_string())?;
    let names: Vec<String> = listing.files.into_iter().map(|f| f.name).collect();
    let file = osc::spec_file_name(&names, &r.source_package);
    let text = osc::cat_file_expanded(&r.source_project, &r.source_package, &file, rev, emit)
        .map_err(|e| e.to_string())?;
    spec_version(&text)
}

/// The cleaned `Version:` of a spec text.
fn spec_version(text: &str) -> Result<String, String> {
    if text.len() > SPEC_CAP {
        return Err(format!("its spec is larger than {SPEC_CAP} bytes"));
    }
    ap_build::spec::parse_header(text)
        .and_then(|h| ap_build::update::clean_version(&h.version))
        .ok_or_else(|| "its spec has no readable Name: and Version:".to_string())
}

/// Whether request `id` is still open, read again now: a kept list can name a
/// request that was declined or revoked since. `Err` says why it is not confirmed.
fn confirm_open(id: &str, read: &dyn Fn(&str) -> Result<String, String>) -> Result<(), String> {
    let doc = read(id)?;
    match osc::parse_open_requests(&doc)
        .into_iter()
        .find(|r| r.id == id)
    {
        Some(r) if matches!(r.state.as_str(), "new" | "review") => Ok(()),
        Some(r) => Err(format!("request {id} is now {}", r.state)),
        None => Err(format!("request {id} could not be read")),
    }
}

/// Whether an open request already updates `item`. When it does, the item is
/// listed under "already requested", in the dry run and in the summary alike,
/// and nothing is branched for it. A request with an older version is noted.
pub(super) fn settled_as_requested(
    item: &Item,
    a: &UpdateProjectArgs,
    open: &mut OpenRequests,
    ctx: &Ctx<'_>,
    report: &mut Report,
) -> bool {
    if !applies(item, a) {
        return false;
    }
    let emit = Emit::run(ctx.reporter.as_ref());
    let requests = open.to_package(
        &item.origin,
        &item.pkg,
        &|origin| osc::open_requests_to_project(origin, emit).map_err(|e| e.to_string()),
        &|w| ctx.warn(w),
    );
    let verdict = decide(item, a, requests.ok_or("not read"), &|r| {
        source_version(r, emit)
    });
    for note in &verdict.notes {
        ctx.say(format!("{}: {note}", item.pkg));
    }
    let Some(found) = verdict.skip else {
        return false;
    };
    // The kept list may be stale: a skip is made only for a request still open.
    let still_open = confirm_open(&found.id, &|id| {
        osc::request_document(id, emit).map_err(|e| e.to_string())
    });
    if let Err(why) = still_open {
        ctx.say(format!("{}: not skipped, {why}", item.pkg));
        return false;
    }
    let why = update::SkipReason::AlreadyRequested(found).to_string();
    ctx.say(format!("{}: {why}", item.pkg));
    report.requested.push(format!("{}: {why}", item.pkg));
    true
}

#[cfg(test)]
#[path = "requested_tests.rs"]
mod tests;
