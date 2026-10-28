//! The data and the text of `update`: which packages to update, what a
//! dry run prints, and the end-of-run summary.
//!
//! The OBS and pipeline work that produces these lives in the CLI; this module
//! has none of it, so the plan and report formats are testable on their own.

use crate::fix_flags::FixFlag;
use crate::osc::{self, PathAddition};
use crate::risk_flags::RiskFlag;
use crate::update::{Candidate, Decision, Ecosystem};

/// One package to branch and update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The package's name in `origin`, in the spelling OBS lists.
    pub pkg: String,
    /// The project it is branched from, and where its submit request goes.
    pub origin: String,
    pub ecosystem: Ecosystem,
    pub old_version: String,
    pub new_version: String,
    /// The package this one was pulled in for (`--update-deps`).
    pub dependency_of: Option<String>,
    /// What `lint` found wrong with the package, one line each.
    /// Empty for an update, where `old_version` and `new_version` differ.
    pub findings: Vec<String>,
}

/// Whether `pkg` is named by `list`, compared the way the rest of the tool
/// compares package names, so `Foo_Bar` finds `foo-bar`.
pub fn name_listed(list: &[String], pkg: &str) -> bool {
    let key = osc::fuzzy_key(pkg);
    list.iter().any(|n| osc::fuzzy_key(n) == key)
}

/// The package to branch for a name `osc ls` listed: a multibuild flavor
/// (`apache-arrow:python-pyarrow`) is part of its source package.
pub fn branch_name(listed: &str) -> &str {
    listed.split(':').next().unwrap_or(listed)
}

/// The packages of a plan that have a newer upstream release, minus
/// `exclude`, each once.
pub fn items_from_plan(origin: &str, candidates: &[Candidate], exclude: &[String]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for c in candidates {
        let Decision::Update {
            ecosystem,
            old_version,
            new_version,
        } = &c.decision
        else {
            continue;
        };
        let pkg = branch_name(&c.rpm_name);
        // A version found in a release listing or on Repology is reported, but
        // the pipeline cannot build from what they name.
        if !ecosystem.can_build() || name_listed(exclude, pkg) || items.iter().any(|i| i.pkg == pkg)
        {
            continue;
        }
        items.push(Item {
            pkg: pkg.to_string(),
            origin: origin.to_string(),
            ecosystem: ecosystem.clone(),
            old_version: old_version.clone(),
            new_version: new_version.clone(),
            dependency_of: None,
            findings: Vec::new(),
        });
    }
    items
}

/// Whether the build pipeline would build `item` under the name it already
/// has. A PyPI package is named by the pipeline from its PyPI name
/// (`python-<name>`); one that is called something else would be built as a
/// second package next to the branched one. A repository package is given its
/// name explicitly, so it never mismatches.
pub fn check_pipeline_name(item: &Item) -> Result<(), String> {
    match &item.ecosystem {
        Ecosystem::Pypi { pypi_name } => {
            let built = osc::rpm_name(pypi_name);
            if osc::fuzzy_key(&built) == osc::fuzzy_key(&item.pkg) {
                Ok(())
            } else {
                Err(format!(
                    "the build pipeline would name it {built}, not {}, and create a second package",
                    item.pkg
                ))
            }
        }
        Ecosystem::GitRepo { .. } => Ok(()),
        // Only reported, never built, so there is no name to mismatch.
        Ecosystem::Index { .. } | Ecosystem::Repology { .. } => Ok(()),
    }
}

/// What a dry run knows.
pub struct DryRun<'a> {
    pub source: &'a str,
    pub branch_project: &'a str,
    pub branch_exists: bool,
    pub items: &'a [Item],
    /// Build paths that would be added to the branch project.
    pub paths: &'a [PathAddition],
    pub submit: bool,
}

/// What a dry run prints after the update plan: the branch project, the build
/// paths that would be added, and the requests that would be created.
pub fn dry_run_lines(d: &DryRun<'_>) -> Vec<String> {
    let mut out = vec![format!(
        "dry run: nothing is branched, built, edited or submitted; {} stays untouched",
        d.source
    )];
    out.push(format!(
        "branch project: {} ({})",
        d.branch_project,
        if d.branch_exists {
            "exists"
        } else {
            "would be created by the first branch"
        }
    ));
    out.push(format!(
        "packages to branch and update ({}):",
        d.items.len()
    ));
    for i in d.items {
        out.push(format!(
            "  {}  {} -> {}  (from {}{}{})",
            i.pkg,
            i.old_version,
            i.new_version,
            i.origin,
            i.dependency_of
                .as_ref()
                .map(|p| format!(", needed by {p}"))
                .unwrap_or_default(),
            match crate::branch_revision::direction_note(&i.old_version, &i.new_version) {
                "" => String::new(),
                note => format!(", {note}"),
            }
        ));
    }
    if d.paths.is_empty() {
        out.push("extra build paths: none needed".to_string());
    } else {
        out.push(format!("extra build paths ({}):", d.paths.len()));
        for p in d.paths {
            out.push(format!(
                "  {}: + {}/{}",
                p.repository, p.project, p.path_repository
            ));
        }
    }
    if d.submit {
        out.push(format!(
            "submit requests that would be created, once each build succeeds ({}):",
            d.items.len()
        ));
        for i in d.items {
            out.push(format!(
                "  {}/{} -> {}/{}",
                d.branch_project, i.pkg, i.origin, i.pkg
            ));
        }
    } else {
        out.push("submit requests: none (--submit not given)".to_string());
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    /// Built in the branch project at the new version.
    Updated,
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
fn failed_submits(results: &[ItemResult]) -> usize {
    results
        .iter()
        .filter(|r| matches!(r.sr, SrOutcome::Failed(_)))
        .count()
}

/// Whether two version strings name the same release (`1.0` and `1.0.0` do).
fn same_version(a: &str, b: &str) -> bool {
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
        ItemOutcome::Updated => {}
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
const SR_CHANGELOG_MAX: usize = 2000;

/// `text` without control characters (a newline stays; a tab becomes a space)
/// and cut to [`SR_CHANGELOG_MAX`] characters: the text is package-controlled
/// and ends up in a request that people read in a terminal or a web page.
fn sanitize_changelog(text: &str) -> String {
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
/// "Needs review:".
pub fn sr_description(
    item: &Item,
    changes: Option<&str>,
    flags: &[FixFlag],
    risks: &[RiskFlag],
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

/// The end-of-run summary.
pub fn summary_text(
    branch_project: &str,
    results: &[ItemResult],
    paths: &[PathAddition],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::SkipReason;

    #[test]
    fn an_improvement_request_lists_the_findings_not_a_version_bump() {
        let mut i = Item {
            pkg: "libfoo".into(),
            origin: "Base:System".into(),
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/a/b".into(),
            },
            old_version: "1.0".into(),
            new_version: "1.0".into(),
            dependency_of: None,
            findings: vec!["no-check: the spec has no %check".into()],
        };
        let d = sr_description(&i, None, &[], &[]);
        assert!(d.starts_with("Improve libfoo 1.0"), "{d}");
        assert!(d.contains("- no-check: the spec has no %check"));
        assert!(!d.contains("was 1.0"));
        i.findings.clear();
        assert!(sr_description(&i, None, &[], &[]).starts_with("Update libfoo to 1.0 (was 1.0)"));
    }

    fn update(name: &str, old: &str, new: &str) -> Candidate {
        Candidate {
            rpm_name: name.into(),
            decision: Decision::Update {
                ecosystem: Ecosystem::Pypi {
                    pypi_name: name.trim_start_matches("python-").into(),
                },
                old_version: old.into(),
                new_version: new.into(),
            },
        }
    }

    fn item(pkg: &str) -> Item {
        Item {
            pkg: pkg.into(),
            origin: "Base:System".into(),
            ecosystem: Ecosystem::GitRepo {
                url: "https://github.com/a/b".into(),
            },
            old_version: "1".into(),
            new_version: "2".into(),
            dependency_of: None,
            findings: Vec::new(),
        }
    }

    #[test]
    fn only_packages_with_an_update_become_items_and_exclude_applies() {
        let cands = vec![
            update("python-a", "1", "2"),
            update("python-B_c", "1", "2"),
            Candidate {
                rpm_name: "python-d".into(),
                decision: Decision::UpToDate {
                    ecosystem: Ecosystem::Pypi {
                        pypi_name: "d".into(),
                    },
                    version: "1".into(),
                },
            },
            Candidate {
                rpm_name: "e".into(),
                decision: Decision::Skipped(SkipReason::NoSourceLine),
            },
        ];
        let items = items_from_plan("Base:System", &cands, &["python-b-c".to_string()]);
        let names: Vec<&str> = items.iter().map(|i| i.pkg.as_str()).collect();
        assert_eq!(names, ["python-a"]);
        assert_eq!(items[0].origin, "Base:System");
    }

    #[test]
    fn a_multibuild_flavor_is_its_source_package_and_listed_once() {
        let cands = vec![update("foo", "1", "2"), update("foo:python-foo", "1", "2")];
        let items = items_from_plan("P", &cands, &[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].pkg, "foo");
    }

    #[test]
    fn a_pypi_package_the_pipeline_would_name_differently_is_refused() {
        let mut i = item("salt");
        i.ecosystem = Ecosystem::Pypi {
            pypi_name: "salt".into(),
        };
        assert!(check_pipeline_name(&i).unwrap_err().contains("python-salt"));
        i.pkg = "python-Salt".into();
        assert!(check_pipeline_name(&i).is_ok());
        // A repository package is named explicitly.
        assert!(check_pipeline_name(&item("libfoo")).is_ok());
    }

    fn path(repo: &str, project: &str) -> PathAddition {
        PathAddition {
            repository: repo.into(),
            project: project.into(),
            path_repository: repo.into(),
        }
    }

    #[test]
    fn the_dry_run_names_the_branch_the_paths_and_the_requests() {
        let items = [item("libfoo"), {
            let mut d = item("libbar");
            d.origin = "devel:languages:c_c++".into();
            d.dependency_of = Some("libfoo".into());
            d
        }];
        let paths = [path("openSUSE_Tumbleweed", "devel:languages:c_c++")];
        let lines = dry_run_lines(&DryRun {
            source: "Base:System",
            branch_project: "home:u:branches:Base:System",
            branch_exists: false,
            items: &items,
            paths: &paths,
            submit: true,
        });
        assert_eq!(
            lines,
            [
                "dry run: nothing is branched, built, edited or submitted; Base:System stays untouched",
                "branch project: home:u:branches:Base:System (would be created by the first branch)",
                "packages to branch and update (2):",
                "  libfoo  1 -> 2  (from Base:System)",
                "  libbar  1 -> 2  (from devel:languages:c_c++, needed by libfoo)",
                "extra build paths (1):",
                "  openSUSE_Tumbleweed: + devel:languages:c_c++/openSUSE_Tumbleweed",
                "submit requests that would be created, once each build succeeds (2):",
                "  home:u:branches:Base:System/libfoo -> Base:System/libfoo",
                "  home:u:branches:Base:System/libbar -> devel:languages:c_c++/libbar",
            ]
        );
    }

    #[test]
    fn the_dry_run_without_submit_or_paths_says_so() {
        let lines = dry_run_lines(&DryRun {
            source: "S",
            branch_project: "b",
            branch_exists: true,
            items: &[item("x")],
            paths: &[],
            submit: false,
        });
        assert!(lines.contains(&"branch project: b (exists)".to_string()));
        assert!(lines.contains(&"extra build paths: none needed".to_string()));
        assert!(lines.contains(&"submit requests: none (--submit not given)".to_string()));
    }

    fn result(pkg: &str, outcome: ItemOutcome, sr: SrOutcome) -> ItemResult {
        ItemResult {
            item: item(pkg),
            outcome,
            sr,
        }
    }

    const CHANGES: &str = "-------------------------------------------------------------------\n\
Tue Oct 14 10:00:00 UTC 2026 - Dev <dev@example.org>\n\
\n\
- Update to 2.0:\n  * new thing\n  * fixed bug\n\
\n\
-------------------------------------------------------------------\n\
Mon Sep  1 09:00:00 UTC 2026 - Dev <dev@example.org>\n\
\n\
- Update to 1.0\n";

    #[test]
    fn the_newest_changes_entry_loses_its_header_and_older_entries() {
        assert_eq!(
            latest_changes_entry(CHANGES).as_deref(),
            Some("- Update to 2.0:\n  * new thing\n  * fixed bug")
        );
        assert_eq!(latest_changes_entry(""), None);
        assert_eq!(latest_changes_entry("no separator here\n"), None);
    }

    #[test]
    fn a_long_changes_entry_is_cut_on_a_line_boundary() {
        let body: String = (0..400).map(|i| format!("- line number {i}\n")).collect();
        let changes = format!("{}\nDate - Who\n\n{body}", "-".repeat(67));
        let entry = latest_changes_entry(&changes).unwrap();
        assert!(entry.ends_with("\n[...]"), "{entry}");
        assert!(entry.chars().count() < 1300);
        assert!(
            entry
                .lines()
                .rev()
                .nth(1)
                .unwrap()
                .starts_with("- line number")
        );
        assert!(!entry.lines().rev().nth(1).unwrap().ends_with("numb"));
    }

    #[test]
    fn the_description_names_the_versions_and_carries_the_changelog() {
        let mut i = item("foo");
        i.old_version = "1.0".into();
        i.new_version = "2.0".into();
        let d = sr_description(&i, Some("- Update to 2.0"), &[], &[]);
        assert!(d.starts_with("Update foo to 2.0 (was 1.0).\n\nChangelog:\n- Update to 2.0"));
        assert!(!sr_description(&i, None, &[], &[]).contains("Changelog"));
    }

    #[test]
    fn the_flags_come_right_after_the_summary_one_line_each() {
        let mut i = item("foo");
        i.old_version = "1.0".into();
        i.new_version = "2.0".into();
        let flags = vec![
            FixFlag::CheckRemoved,
            FixFlag::WheelSwitch {
                wheel: "foo-2.0-py3-none-any.whl".into(),
            },
        ];
        let d = sr_description(&i, Some("- Update to 2.0"), &flags, &[]);
        assert!(
            d.starts_with(
                "Update foo to 2.0 (was 1.0).\n\nFlagged for review:\n- FLAG: %check removed"
            ),
            "{d}"
        );
        assert!(
            d.contains(
                "\n- FLAG: built from the upstream wheel foo-2.0-py3-none-any.whl, not from source\n\nChangelog:\n"
            ),
            "{d}"
        );
        assert!(!sr_description(&i, None, &[], &[]).contains("Flagged"));
    }

    #[test]
    fn the_risks_follow_the_fix_flags_under_needs_review() {
        let mut i = item("foo");
        i.old_version = "1.0".into();
        i.new_version = "2.0".into();
        let flags = vec![FixFlag::CheckRemoved];
        let risks = vec![RiskFlag::NewScriptlet {
            header: "%post".into(),
        }];
        let d = sr_description(&i, Some("- Update to 2.0"), &flags, &risks);
        assert!(
            d.contains(
                "\n\nFlagged for review:\n- FLAG: %check removed or left without a test: the tests do not run\n\nNeeds review:\n- RISK: new scriptlet: %post\n\nChangelog:\n"
            ),
            "{d}"
        );
        assert!(!sr_description(&i, None, &[], &[]).contains("Needs review"));
    }

    #[test]
    fn the_changelog_in_the_description_has_no_control_characters_and_is_capped() {
        let i = item("foo");
        let d = sr_description(&i, Some("- ok\x1b[31m red\r\n\x07bell\tTab"), &[], &[]);
        assert!(d.contains("- ok[31m red\nbell Tab"), "{d:?}");
        assert!(!d.contains('\x1b') && !d.contains('\r') && !d.contains('\x07'));
        let long = "x".repeat(10_000);
        let d = sr_description(&i, Some(&long), &[], &[]);
        assert!(d.len() < 2500, "{}", d.len());
        assert!(d.contains("[...]"));
    }

    #[test]
    fn only_a_built_package_at_the_new_version_may_be_submitted() {
        use osc::BuildStatus::*;
        let i = item("foo");
        let ok = ItemOutcome::Updated;
        assert_eq!(sr_eligibility(&i, &ok, Some(&Succeeded), Some("2")), Ok(()));
        // 2 and 2.0 are the same release.
        assert_eq!(
            sr_eligibility(&i, &ok, Some(&Succeeded), Some("2.0")),
            Ok(())
        );
        let why = |o: &ItemOutcome, b: Option<&osc::BuildStatus>, v: Option<&str>| {
            sr_eligibility(&i, o, b, v).unwrap_err()
        };
        assert!(
            why(
                &ItemOutcome::Failed("x".into()),
                Some(&Succeeded),
                Some("2")
            )
            .contains("failed")
        );
        assert!(why(&ItemOutcome::Skipped("x".into()), None, None).contains("skipped"));
        assert!(why(&ok, Some(&Failed), Some("2")).contains("failed"));
        assert!(why(&ok, Some(&Building), Some("2")).contains("building"));
        assert!(why(&ok, None, Some("2")).contains("could not be read"));
        assert!(why(&ok, Some(&Succeeded), Some("1")).contains("says 1, not 2"));
        assert!(why(&ok, Some(&Succeeded), None).contains("spec could not be read"));
    }

    #[test]
    fn a_failed_submit_request_fails_the_run() {
        let failed_sr = vec![
            result("a", ItemOutcome::Updated, SrOutcome::Failed("boom".into())),
            result("b", ItemOutcome::Updated, SrOutcome::Failed("boom".into())),
        ];
        assert!(any_failed(&failed_sr));
        assert_eq!(failure_lines(&failed_sr), ["2 submit request(s) failed"]);
        let text = summary_text("home:u:b", &failed_sr, &[], true);
        assert!(text.contains("2 submit request(s) failed\n"), "{text}");
        assert!(text.contains("  a -> Base:System: failed: boom"), "{text}");

        // A package failure and a submit failure are reported separately.
        let both = vec![
            result(
                "a",
                ItemOutcome::Failed("build".into()),
                SrOutcome::NotRequested,
            ),
            result("b", ItemOutcome::Updated, SrOutcome::Failed("boom".into())),
        ];
        assert_eq!(
            failure_lines(&both),
            [
                "1 package(s) failed to update",
                "1 submit request(s) failed"
            ]
        );

        // Requests that were declined, not eligible, or already open are not failures.
        let quiet = vec![
            result("a", ItemOutcome::Updated, SrOutcome::Declined),
            result(
                "b",
                ItemOutcome::Updated,
                SrOutcome::NotEligible("x".into()),
            ),
            result("c", ItemOutcome::Updated, SrOutcome::Existing("7".into())),
            result("d", ItemOutcome::Updated, SrOutcome::Created("1".into())),
        ];
        assert!(!any_failed(&quiet));
        assert!(failure_lines(&quiet).is_empty());
        assert!(!summary_text("home:u:b", &quiet, &[], true).contains("failed\n"));
    }

    #[test]
    fn the_confirmation_lists_each_request_with_its_origin() {
        let a = item("foo");
        let mut b = item("bar");
        b.origin = "devel:x".into();
        assert_eq!(
            sr_confirmation_lines("home:u:b", &[&a, &b]),
            [
                "home:u:b/foo -> Base:System/foo  (1 -> 2)",
                "home:u:b/bar -> devel:x/bar  (1 -> 2)"
            ]
        );
    }

    #[test]
    fn the_summary_counts_every_outcome_and_a_failure_fails_the_run() {
        let results = vec![
            result("a", ItemOutcome::Updated, SrOutcome::Created("42".into())),
            result(
                "b",
                ItemOutcome::Failed("build".into()),
                SrOutcome::NotEligible("failed".into()),
            ),
            result(
                "c",
                ItemOutcome::Skipped("name".into()),
                SrOutcome::NotRequested,
            ),
            result("d", ItemOutcome::Updated, SrOutcome::Existing("7".into())),
        ];
        assert!(any_failed(&results));
        assert!(!any_failed(&results[..1]));
        assert_eq!(failure_lines(&results), ["1 package(s) failed to update"]);
        let text = summary_text("home:u:b", &results, &[path("r", "devel:x")], true);
        for needle in [
            "branch project: home:u:b",
            "updated (2):",
            "  a 1 -> 2",
            "failed (1):",
            "  b: build",
            "skipped (1):",
            "  c: name",
            "submit requests (3):",
            "  a -> Base:System: created request 42",
            "  d -> Base:System: request 7 already open",
            "  b -> Base:System: not submitted: failed",
            "build paths added (1):",
            "  devel:x/r to repository r",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
        assert!(!summary_text("b", &results, &[], false).contains("submit requests"));
    }
}
