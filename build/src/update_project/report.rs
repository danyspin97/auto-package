use super::*;
use crate::arch_check::{self, ArchRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    /// Built in the branch project at the new version.
    Updated,
    /// The branch project already held the new version, so nothing was built.
    AlreadyUpdated,
    Failed(String),
    /// Not attempted, with the reason.
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrOutcome {
    /// `--submit` was not given.
    NotRequested,
    Created(String),
    /// An open request from the branch project already exists.
    Existing(String),
    /// The user did not confirm it.
    Declined,
    /// `--yes` does not approve a request whose change needs a review (see
    /// its risk flags); the run is refused.
    NeedsReview,
    /// Not submitted, with the reason (a failed build, a version mismatch).
    NotEligible(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResult {
    pub item: Item,
    pub outcome: ItemOutcome,
    pub sr: SrOutcome,
}

/// Whether the run counts as failed: a package that could not be updated, or a
/// submit request that could not be made.
pub fn any_failed(results: &[ItemResult]) -> bool {
    !failure_lines(results).is_empty()
}

/// Why the run failed, one line per kind of failure, empty when it did not.
pub fn failure_lines(results: &[ItemResult]) -> Vec<String> {
    let items = results
        .iter()
        .filter(|r| matches!(r.outcome, ItemOutcome::Failed(_)))
        .count();
    let submits = failed_submits(results);
    let mut lines = Vec::new();
    if items > 0 {
        lines.push(format!("{items} package(s) failed to update"));
    }
    if submits > 0 {
        lines.push(format!("{submits} submit request(s) failed"));
    }
    lines
}

/// How many submit requests could not be made.
pub(super) fn failed_submits(results: &[ItemResult]) -> usize {
    results
        .iter()
        .filter(|r| matches!(r.sr, SrOutcome::Failed(_)))
        .count()
}

/// Whether two version strings name the same release (`1.0` and `1.0.0` do).
pub(super) fn same_version(a: &str, b: &str) -> bool {
    use ap_ecosystem::pypi::{VersionCompare, compare_pep440};
    a == b
        || (compare_pep440(a, b) == VersionCompare::NotNewer
            && compare_pep440(b, a) == VersionCompare::NotNewer)
}

/// Why a package must not get a submit request, or `Ok` when it may.
///
/// A request is only for a package that built in the branch project, from a
/// spec that is at the new version: a build that succeeded on the old sources
/// (the update never landed) is not an update to submit. `build` is what OBS
/// reports for the package, `None` when it could not be read; `spec_version`
/// the version in the branch project's spec.
pub fn sr_eligibility(
    item: &Item,
    outcome: &ItemOutcome,
    build: Option<&osc::BuildStatus>,
    spec_version: Option<&str>,
) -> Result<(), String> {
    match outcome {
        ItemOutcome::Updated | ItemOutcome::AlreadyUpdated => {}
        ItemOutcome::Failed(_) => return Err("its update failed".to_string()),
        ItemOutcome::Skipped(_) => return Err("it was skipped".to_string()),
    }
    match build {
        Some(osc::BuildStatus::Succeeded) => {}
        Some(other) => {
            return Err(format!(
                "its build in the branch project is {other}, not succeeded"
            ));
        }
        None => return Err("its build status in the branch project could not be read".to_string()),
    }
    match spec_version {
        Some(v) if same_version(v, &item.new_version) => Ok(()),
        Some(v) => Err(format!(
            "the branch project's spec says {v}, not {}",
            item.new_version
        )),
        None => Err("the branch project's spec could not be read".to_string()),
    }
}

/// The newest entry of a `.changes` file, without its date and author line,
/// shortened to something a request description can carry.
pub fn latest_changes_entry(changes: &str) -> Option<String> {
    const MAX: usize = 1200;
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    // The file starts with a separator; skip anything before it.
    let mut lines: Vec<&str> = Vec::new();
    let mut started = false;
    for line in changes.split('\n').map(str::trim_end) {
        if is_separator(line) {
            if started && !lines.iter().all(|l| l.trim().is_empty()) {
                break;
            }
            started = true;
            lines.clear();
        } else if started {
            lines.push(line);
        }
    }
    // The first line of an entry is `date - author <mail>`.
    let body: Vec<&str> = lines
        .into_iter()
        .skip_while(|l| l.trim().is_empty())
        .skip(1)
        .collect();
    let text = body.join("\n");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX {
        return Some(text.to_string());
    }
    let cut: String = text.chars().take(MAX).collect();
    // End on a whole line.
    let cut = cut.rsplit_once('\n').map_or(cut.as_str(), |(head, _)| head);
    Some(format!("{}\n[...]", cut.trim_end()))
}

/// Longest changelog text a request description carries.
pub(super) const SR_CHANGELOG_MAX: usize = 2000;

/// `text` without control characters (a newline stays; a tab becomes a space)
/// and cut to [`SR_CHANGELOG_MAX`] characters: the text is package-controlled
/// and ends up in a request that people read in a terminal or a web page.
pub(super) fn sanitize_changelog(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    if clean.chars().count() <= SR_CHANGELOG_MAX {
        return clean;
    }
    let cut: String = clean.chars().take(SR_CHANGELOG_MAX).collect();
    format!("{}\n[...]", cut.trim_end())
}

/// The description of the submit request for `item`. `flags` are what the
/// package's fix loop changed (skipped tests, a wheel, a blocked turn); they
/// come right after the summary, one line each, so a reviewer sees them first.
/// `risks` (what the change adds that needs a look) follow them under
/// "Needs review:". `not_final` lists, per architecture, the results still
/// going when the request is made (see `gate::not_final_text`).
pub fn sr_description(
    item: &Item,
    changes: Option<&str>,
    flags: &[FixFlag],
    risks: &[RiskFlag],
    not_final: &[String],
) -> String {
    let mut out = if item.findings.is_empty() {
        format!(
            "Update {} to {} (was {}).",
            item.pkg, item.new_version, item.old_version
        )
    } else {
        format!(
            "Improve {} {}; the quality checks found:\n- {}",
            item.pkg,
            item.new_version,
            item.findings.join("\n- ")
        )
    };
    if !flags.is_empty() {
        let lines: Vec<String> = flags
            .iter()
            .map(|f| format!("- {}", sanitize_changelog(&f.line())))
            .collect();
        out.push_str("\n\nFlagged for review:\n");
        out.push_str(&lines.join("\n"));
    }
    if !risks.is_empty() {
        let lines: Vec<String> = risks
            .iter()
            .map(|r| format!("- {}", sanitize_changelog(&r.line())))
            .collect();
        out.push_str("\n\nNeeds review:\n");
        out.push_str(&lines.join("\n"));
    }
    if !not_final.is_empty() {
        let lines: Vec<String> = not_final
            .iter()
            .map(|n| format!("- {}", sanitize_changelog(n)))
            .collect();
        out.push_str("\n\nNot final at submit time:\n");
        out.push_str(&lines.join("\n"));
    }
    if let Some(c) = changes {
        out.push_str("\n\nChangelog:\n");
        out.push_str(&sanitize_changelog(c));
    }
    out.push_str("\n\nBuilt in the branch project by auto-package.");
    out
}

/// The list shown before submit requests are created, one line each.
pub fn sr_confirmation_lines(branch_project: &str, items: &[&Item]) -> Vec<String> {
    items
        .iter()
        .map(|i| {
            format!(
                "{}/{} -> {}/{}  ({} -> {})",
                branch_project, i.pkg, i.origin, i.pkg, i.old_version, i.new_version
            )
        })
        .collect()
}

/// The "Other architectures" section of the end-of-run summary: empty when the
/// check found nothing to report.
pub fn arch_section(rows: &[ArchRow]) -> String {
    use std::fmt::Write as _;
    let lines = arch_check::section_lines(rows);
    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "Other architectures ({}):", lines.len());
    for l in lines {
        let _ = writeln!(out, "  {l}");
    }
    out
}

/// The packages left out because an open submit request already updates them:
/// information, not a failure and not a maintainer's decision. Empty when there
/// are none.
pub fn requested_lines(requested: &[String]) -> Vec<String> {
    if requested.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("already requested ({}):", requested.len())];
    out.extend(requested.iter().map(|r| format!("  {r}")));
    out
}

/// [`requested_lines`] as a section of the end-of-run summary.
pub fn requested_section(requested: &[String]) -> String {
    requested_lines(requested)
        .into_iter()
        .map(|l| l + "\n")
        .collect()
}

/// The end-of-run summary.
pub fn summary_text(
    branch_project: &str,
    results: &[ItemResult],
    paths: &[PathAddition],
    missing: &[MissingRequirement],
    maintainer: &[String],
    submit: bool,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let section = |out: &mut String, title: &str, lines: Vec<String>| {
        let _ = writeln!(out, "{title} ({}):", lines.len());
        for l in lines {
            let _ = writeln!(out, "  {l}");
        }
    };
    let _ = writeln!(out, "\nupdate summary");
    let _ = writeln!(out, "branch project: {branch_project}");
    section(
        &mut out,
        "updated",
        results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::Updated)
            .map(|r| {
                format!(
                    "{} {} -> {}",
                    r.item.pkg, r.item.old_version, r.item.new_version
                )
            })
            .collect(),
    );
    section(
        &mut out,
        "already updated in the branch (not rebuilt)",
        results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::AlreadyUpdated)
            .map(|r| format!("{} {}", r.item.pkg, r.item.new_version))
            .collect(),
    );
    section(
        &mut out,
        "failed",
        results
            .iter()
            .filter_map(|r| match &r.outcome {
                ItemOutcome::Failed(why) => Some(format!("{}: {why}", r.item.pkg)),
                _ => None,
            })
            .collect(),
    );
    section(
        &mut out,
        "skipped",
        results
            .iter()
            .filter_map(|r| match &r.outcome {
                ItemOutcome::Skipped(why) => Some(format!("{}: {why}", r.item.pkg)),
                _ => None,
            })
            .collect(),
    );
    section(&mut out, "needs the maintainer", maintainer.to_vec());
    section(
        &mut out,
        "requirements no provider satisfies",
        requirement_lines(missing),
    );
    if submit {
        section(
            &mut out,
            "submit requests",
            results
                .iter()
                .filter_map(|r| {
                    let what = match &r.sr {
                        SrOutcome::Created(id) => format!("created request {id}"),
                        SrOutcome::Existing(id) => format!("request {id} already open"),
                        SrOutcome::Declined => "not confirmed".to_string(),
                        SrOutcome::NeedsReview => {
                            "not submitted: --yes refused the run, a package needs a review"
                                .to_string()
                        }
                        SrOutcome::NotEligible(why) => format!("not submitted: {why}"),
                        SrOutcome::Failed(why) => format!("failed: {why}"),
                        SrOutcome::NotRequested => return None,
                    };
                    Some(format!("{} -> {}: {what}", r.item.pkg, r.item.origin))
                })
                .collect(),
        );
        let submits = failed_submits(results);
        if submits > 0 {
            let _ = writeln!(out, "{submits} submit request(s) failed");
        }
    }
    section(
        &mut out,
        "build paths added",
        paths
            .iter()
            .map(|p| {
                format!(
                    "{}/{} to repository {}",
                    p.project, p.path_repository, p.repository
                )
            })
            .collect(),
    );
    out
}
