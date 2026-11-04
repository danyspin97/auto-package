//! The reads of a review, in order: the request, then for each submit action
//! both package listings, the texts the checks read, the diff and the other
//! open requests. A required read that fails is a [`ReviewError`]; an optional
//! one is a `review.incomplete` finding.

use std::collections::BTreeMap;

use super::fetch::{ReadFailure, RequestObs};
use super::request::{
    ActionSource, ActionTarget, RequestDoc, competing_findings, parse_request, request_findings,
};
use super::{Finding, Severity, ids};
use crate::osc::{DiffSpec, Listing};
use crate::update::pick_spec_filename;

/// A text file over this size is not read.
pub const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;

/// A read the checks need failed: the review cannot be completed, and no
/// report is made of it (exit status 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewError(pub String);

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReviewError {}

/// A check that did not run, or only in part, on purpose.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skipped {
    pub check: String,
    pub why: String,
}

/// One side of a request: the listing and the texts of the files the checks
/// read, by name.
#[derive(Debug, Clone, Default)]
pub struct Side {
    pub listing: Listing,
    pub texts: BTreeMap<String, String>,
    pub spec_name: Option<String>,
}

impl Side {
    pub fn spec(&self) -> Option<&str> {
        self.texts
            .get(self.spec_name.as_deref()?)
            .map(String::as_str)
    }

    /// The `.changes` file named after the spec, or the only one there is.
    pub fn changes_name(&self) -> Option<String> {
        let own = self
            .spec_name
            .as_deref()
            .and_then(|s| s.strip_suffix(".spec"))
            .map(|stem| format!("{stem}.changes"));
        let names = || self.listing.files.iter().map(|f| f.name.as_str());
        if let Some(own) = own.filter(|o| names().any(|n| n == o)) {
            return Some(own);
        }
        let mut all = names().filter(|n| n.ends_with(".changes"));
        let first = all.next()?;
        all.next().is_none().then(|| first.to_string())
    }

    pub fn changes(&self) -> Option<&str> {
        self.texts.get(&self.changes_name()?).map(String::as_str)
    }

    pub fn file_names(&self) -> Vec<String> {
        self.listing.files.iter().map(|f| f.name.clone()).collect()
    }

    pub fn service(&self) -> Option<&str> {
        self.texts.get("_service").map(String::as_str)
    }

    /// Whether the side holds `name`, listed.
    pub fn lists(&self, name: &str) -> bool {
        self.listing.files.iter().any(|f| f.name == name)
    }
}

/// The file names the request adds, changes and removes, by listing and md5.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFiles {
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

pub fn changed_files(old: &Listing, new: &Listing) -> ChangedFiles {
    let md5 = |l: &Listing| -> BTreeMap<String, String> {
        l.files
            .iter()
            .map(|f| (f.name.clone(), f.md5.clone()))
            .collect()
    };
    let (old, new) = (md5(old), md5(new));
    let mut out = ChangedFiles::default();
    for (name, sum) in &new {
        match old.get(name) {
            None => out.added.push(name.clone()),
            Some(prev) if prev != sum => out.changed.push(name.clone()),
            Some(_) => {}
        }
    }
    out.removed = old
        .keys()
        .filter(|n| !new.contains_key(*n))
        .cloned()
        .collect();
    out
}

/// Everything read for one submit action.
#[derive(Debug, Clone)]
pub struct ActionInputs {
    pub source: ActionSource,
    pub target: ActionTarget,
    pub old: Side,
    pub new: Side,
    pub target_missing: bool,
    pub files: ChangedFiles,
    pub diff: Option<String>,
    /// The `srcmd5` of the source revision the action names, from its listing.
    pub source_srcmd5: Option<String>,
    /// The source's current `srcmd5`.
    pub current_srcmd5: Option<String>,
    pub findings: Vec<Finding>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone)]
pub struct Inputs {
    pub doc: RequestDoc,
    /// The findings of the request as a whole.
    pub findings: Vec<Finding>,
    pub actions: Vec<ActionInputs>,
}

/// Whether the checks read this file of a package.
fn is_read(name: &str) -> bool {
    name.ends_with(".spec")
        || name.ends_with(".changes")
        || name == "_service"
        || name.ends_with(".patch")
        || name.ends_with(".diff")
        || name.ends_with("rpmlintrc")
}

/// An optional read that failed: a note, the review goes on without it.
fn incomplete(package: &str, what: &str, why: &ReadFailure) -> Finding {
    Finding::check(
        ids::REVIEW_INCOMPLETE,
        Severity::Note,
        package,
        &format!("{what} could not be read and was not checked: {why}"),
    )
}

/// Read the texts of one side of an action. Each is read once: a file whose md5
/// the other side has with the same name is taken from `other`. A text the
/// checks need that cannot be read, or is over the size cap, ends the review:
/// judging the rest without it would report a change as a missing file.
fn read_side(
    obs: &dyn RequestObs,
    project: &str,
    pkg: &str,
    rev: Option<&str>,
    listing: Listing,
    other: Option<&Side>,
) -> Result<Side, ReviewError> {
    let names: Vec<String> = listing.files.iter().map(|f| f.name.clone()).collect();
    let mut side = Side {
        spec_name: pick_spec_filename(pkg, &names),
        listing,
        texts: BTreeMap::new(),
    };
    for f in side
        .listing
        .files
        .clone()
        .iter()
        .filter(|f| is_read(&f.name))
    {
        if f.size > MAX_TEXT_BYTES {
            return Err(ReviewError(format!(
                "{} of {project}/{pkg} is over {} MiB and is not read, so the review cannot be completed",
                f.name,
                MAX_TEXT_BYTES / (1024 * 1024)
            )));
        }
        let same = other.and_then(|o| {
            let theirs = o.listing.files.iter().find(|g| g.name == f.name)?;
            (theirs.md5 == f.md5).then(|| o.texts.get(&f.name).cloned())?
        });
        let text = match same {
            Some(text) => Ok(text),
            None => obs.file(project, pkg, &f.name, rev),
        };
        match text {
            Ok(text) => {
                side.texts.insert(f.name.clone(), text);
            }
            Err(e) => return Err(required(&format!("{} of {project}/{pkg}", f.name), &e)),
        }
    }
    Ok(side)
}

fn required(what: &str, e: &ReadFailure) -> ReviewError {
    match e.http {
        Some(401 | 403) => ReviewError(format!(
            "osc is not configured for this API (HTTP {}) while reading {what}",
            e.http.unwrap_or(0)
        )),
        _ => ReviewError(format!("cannot read {what}: {e}")),
    }
}

/// Read what the review needs for request `id`.
pub fn gather(obs: &dyn RequestObs, id: &str) -> Result<Inputs, ReviewError> {
    let xml = obs.request(id).map_err(|e| match e.http {
        Some(404) => ReviewError(format!("request {id} does not exist")),
        _ => required(&format!("request {id}"), &e),
    })?;
    let doc = parse_request(&xml).map_err(|e| ReviewError(format!("request {id}: {e}")))?;
    if doc.submit_actions().next().is_none() {
        return Err(ReviewError(format!(
            "request {id} has no submit action to review"
        )));
    }
    let findings = request_findings(&doc);
    let mut actions = Vec::new();
    for (source, target) in doc.submit_actions() {
        actions.push(gather_action(obs, &doc, source, target)?);
    }
    Ok(Inputs {
        doc,
        findings,
        actions,
    })
}

fn gather_action(
    obs: &dyn RequestObs,
    doc: &RequestDoc,
    source: &ActionSource,
    target: &ActionTarget,
) -> Result<ActionInputs, ReviewError> {
    let rev = source.rev.as_deref();
    let (sp, spkg) = (source.project.as_str(), source.package.as_str());
    let (tp, tpkg) = (target.project.as_str(), target.package.as_str());
    let new_listing = obs
        .listing(sp, spkg, rev)
        .map_err(|e| required(&format!("the source listing of {sp}/{spkg}"), &e))?
        .ok_or_else(|| {
            ReviewError(format!(
                "the source {sp}/{spkg}{} no longer exists (request is {})",
                rev.map_or(String::new(), |r| format!("@{r}")),
                doc.state.name
            ))
        })?;
    let old_listing = obs
        .listing(tp, tpkg, None)
        .map_err(|e| required(&format!("the target listing of {tp}/{tpkg}"), &e))?;
    let target_missing = old_listing.is_none();
    let old_listing = old_listing.unwrap_or_default();
    let files = changed_files(&old_listing, &new_listing);
    let mut out = ActionInputs {
        source: source.clone(),
        target: target.clone(),
        old: Side::default(),
        new: Side::default(),
        target_missing,
        files,
        diff: None,
        source_srcmd5: new_listing.srcmd5.clone(),
        current_srcmd5: None,
        findings: Vec::new(),
        skipped: Vec::new(),
    };
    let old = read_side(obs, tp, tpkg, None, old_listing, None)?;
    let new = read_side(obs, sp, spkg, rev, new_listing, Some(&old))?;
    out.old = old;
    out.new = new;
    if out.new.spec().is_none() {
        return Err(ReviewError(format!(
            "the source {sp}/{spkg} has no readable spec file"
        )));
    }
    let spec = DiffSpec {
        source_project: sp,
        source_package: spkg,
        rev,
        target_project: tp,
        target_package: tpkg,
        target_missing,
    };
    match obs.diff(&spec) {
        Ok(d) => out.diff = Some(d),
        Err(e) => out.findings.push(incomplete(tpkg, "the source diff", &e)),
    }
    match obs.current_srcmd5(sp, spkg) {
        Ok(now) => out.current_srcmd5 = Some(now),
        Err(e) => out
            .findings
            .push(incomplete(tpkg, "the current revision of the source", &e)),
    }
    match obs.open_requests_to(tp, tpkg) {
        Ok(open) => out
            .findings
            .extend(competing_findings(&open, &doc.id, tpkg)),
        Err(e) => out
            .findings
            .push(incomplete(tpkg, "the other open requests", &e)),
    }
    Ok(out)
}
