//! A review from the request id to the report: every read, then every check,
//! in the order of the plan. The reads of OBS go through [`RequestObs`]; the
//! lookups outside OBS (the planner, Repology, the target's repository) are
//! given in [`Deps`] so the whole runs against fixtures.

use std::collections::BTreeMap;
use std::path::Path;

use super::archive::archive_members;
use super::build_check::build_report;
use super::changes_checks::{ChangesCtx, changes_findings};
use super::downloads::{Downloads, download_disabled, download_sources};
use super::fetch::RequestObs;
use super::gather::{ActionInputs, ReviewError, Skipped, gather};
use super::patch_checks::patch_findings;
use super::report::{ActionReport, Report, RequestSummary, SCHEMA, exit_code};
use super::risk::risk_findings;
use super::signature_checks::{keyring_findings, signature_findings};
use super::source_checks::source_findings;
use super::spec_checks::spec_findings;
use super::upstream_check::upstream_report;
use super::{Finding, Severity};
use crate::risk_flags::patch_names;
use crate::update::Candidate;

/// The lookups a review makes outside OBS's source reads.
pub struct Deps<'a> {
    pub plan: &'a dyn Fn(&ActionInputs) -> Result<Candidate, String>,
    pub repology: &'a dyn Fn(&str) -> Result<String, String>,
    /// Whether the target project's repository defines `%ldconfig_scriptlets`.
    pub ldconfig_macro: &'a dyn Fn(&str) -> bool,
    /// Whether a host may be contacted for a lookup: the allowed-hosts list.
    pub host_allowed: &'a dyn Fn(&str) -> bool,
}

pub struct Options<'a> {
    pub id: &'a str,
    /// Download the signed archives and the main archive.
    pub download: bool,
    /// Where the downloads go; removed by the caller.
    pub scratch: &'a Path,
    pub fail_on: Severity,
    pub fail_on_agent: bool,
    /// The build of auto-package, for the report.
    pub build: &'a str,
}

/// The `.spec` or a patch changed, was added or removed.
fn packaging_changed(a: &ActionInputs) -> bool {
    let f = &a.files;
    f.changed
        .iter()
        .chain(&f.added)
        .chain(&f.removed)
        .any(|n| n.ends_with(".spec") || n.ends_with(".patch") || n.ends_with(".diff"))
}

fn version_of(spec: Option<&str>) -> Option<String> {
    crate::spec::parse_header(spec?).map(|h| h.version)
}

/// Every check of one action. `index` is its place in the request, which
/// keeps two actions on the same package name apart. An error is a read the
/// checks need that failed.
fn check_action(
    obs: &dyn RequestObs,
    a: &ActionInputs,
    o: &Options<'_>,
    deps: &Deps<'_>,
    created: &str,
    index: usize,
) -> Result<ActionReport, ReviewError> {
    let package = a.target.package.as_str();
    let mut findings = a.findings.clone();
    let mut skipped: Vec<Skipped> = a.skipped.clone();
    let (base_spec, new_spec) = (a.old.spec(), a.new.spec());
    let (old_version, new_version) = (version_of(base_spec), version_of(new_spec));
    let spec_name = a.new.spec_name.as_deref().unwrap_or("the spec");
    if let Some(new_spec) = new_spec {
        let ldconfig = (deps.ldconfig_macro)(&a.target.project);
        findings.extend(spec_findings(
            package, spec_name, base_spec, new_spec, ldconfig,
        ));
        let names = |s: Option<&str>| s.map(patch_names).unwrap_or_default();
        let (before, after) = (names(base_spec), names(Some(new_spec)));
        let added: Vec<String> = after
            .iter()
            .filter(|n| !before.contains(n))
            .cloned()
            .collect();
        let removed: Vec<String> = before
            .iter()
            .filter(|n| !after.contains(n))
            .cloned()
            .collect();
        let changes_name = a
            .new
            .changes_name()
            .unwrap_or_else(|| format!("{package}.changes"));
        findings.extend(changes_findings(&ChangesCtx {
            package,
            file: &changes_name,
            base: a.old.changes(),
            new: a.new.changes(),
            base_version: old_version.as_deref(),
            new_version: new_version.as_deref().unwrap_or(""),
            packaging_changed: packaging_changed(a),
            patches_added: &added,
            patches_removed: &removed,
            created,
        }));
    }
    findings.extend(patch_findings(package, &a.old, &a.new));
    let downloads = if o.download {
        let dir = o.scratch.join(format!("{index}-{package}"));
        download_sources(obs, a, &dir)
    } else {
        skipped.extend(download_disabled(a));
        Downloads::default()
    };
    if let Some(why) = &downloads.required_failure {
        return Err(ReviewError(format!(
            "{package}: {why}, so its signatures cannot be checked and the review cannot be completed"
        )));
    }
    findings.extend(downloads.findings.clone());
    skipped.extend(downloads.skipped.clone());
    let (archive, unchecked) = match downloads.main_archive.as_deref().map(archive_members) {
        None => (None, "it was not downloaded".to_string()),
        Some(Ok(files)) => (Some(files), String::new()),
        Some(Err(e)) => (None, format!("its file list could not be read: {e}")),
    };
    findings.extend(risk_findings(
        package,
        &a.old,
        &a.new,
        archive.as_deref(),
        &unchecked,
    ));
    let mut archives: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &downloads.fetched {
        if super::downloads::is_vendor(name) {
            match archive_members(&downloads.dir.join(name)) {
                Ok(files) => {
                    archives.insert(name.clone(), files);
                }
                Err(e) => findings.push(Finding::check(
                    super::ids::REVIEW_INCOMPLETE,
                    Severity::Note,
                    package,
                    &format!("{name} could not be listed and was not checked for a target/ directory: {e}"),
                )),
            }
        }
    }
    findings.extend(source_findings(
        package,
        &a.old,
        &a.new,
        &a.files.added,
        &archives,
    ));
    findings.extend(if o.download {
        signature_findings(a, &downloads)
    } else {
        keyring_findings(a)
    });
    findings.extend(build_report(obs, a).map_err(|e| ReviewError(format!("{package}: {e}")))?);
    let (found, not_checked) = upstream_report(a, deps.plan, deps.repology, deps.host_allowed);
    findings.extend(found);
    skipped.extend(not_checked);
    findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    Ok(ActionReport {
        source: a.source.clone(),
        target: a.target.clone(),
        old_version,
        new_version,
        changed: a.files.changed.clone(),
        added: a.files.added.clone(),
        removed: a.files.removed.clone(),
        findings,
        skipped,
    })
}

/// Review request `o.id`: read it, run every check, build the report. An error
/// is a required read that failed.
pub fn review(
    obs: &dyn RequestObs,
    o: &Options<'_>,
    deps: &Deps<'_>,
) -> Result<Report, ReviewError> {
    let inputs = gather(obs, o.id)?;
    let actions = inputs
        .actions
        .iter()
        .enumerate()
        .map(|(i, a)| check_action(obs, a, o, deps, &inputs.doc.state.created, i))
        .collect::<Result<Vec<ActionReport>, ReviewError>>()?;
    let doc = &inputs.doc;
    let mut request_findings: Vec<Finding> = inputs.findings;
    request_findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    let mut report = Report {
        schema: SCHEMA,
        auto_package: o.build.to_string(),
        request: RequestSummary {
            id: doc.id.clone(),
            state: doc.state.name.clone(),
            creator: doc.creator.clone(),
            created: doc.state.created.clone(),
            description: doc.description.clone(),
            findings: request_findings,
        },
        actions,
        agent: None,
        fail_on: o.fail_on,
        fail_on_agent: o.fail_on_agent,
        exit: 0,
    };
    report.exit = exit_code(&report);
    Ok(report)
}
