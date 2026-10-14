//! The data and the text of `update-project`: which packages to update, what a
//! dry run prints, and the end-of-run summary.
//!
//! The OBS and pipeline work that produces these lives in the CLI; this module
//! has none of it, so the plan and report formats are testable on their own.

use crate::osc::{self, PathAddition};
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
        if name_listed(exclude, pkg) || items.iter().any(|i| i.pkg == pkg) {
            continue;
        }
        items.push(Item {
            pkg: pkg.to_string(),
            origin: origin.to_string(),
            ecosystem: ecosystem.clone(),
            old_version: old_version.clone(),
            new_version: new_version.clone(),
            dependency_of: None,
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
    out.push(format!("packages to branch and update ({}):", d.items.len()));
    for i in d.items {
        out.push(format!(
            "  {}  {} -> {}  (from {}{})",
            i.pkg,
            i.old_version,
            i.new_version,
            i.origin,
            i.dependency_of
                .as_ref()
                .map(|p| format!(", needed by {p}"))
                .unwrap_or_default()
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

/// Whether the run counts as failed: a package that could not be updated.
pub fn any_failed(results: &[ItemResult]) -> bool {
    results
        .iter()
        .any(|r| matches!(r.outcome, ItemOutcome::Failed(_)))
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
    let _ = writeln!(out, "\nupdate-project summary");
    let _ = writeln!(out, "branch project: {branch_project}");
    section(
        &mut out,
        "updated",
        results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::Updated)
            .map(|r| format!("{} {} -> {}", r.item.pkg, r.item.old_version, r.item.new_version))
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
                        SrOutcome::NotEligible(why) => format!("not submitted: {why}"),
                        SrOutcome::Failed(why) => format!("failed: {why}"),
                        SrOutcome::NotRequested => return None,
                    };
                    Some(format!("{} -> {}: {what}", r.item.pkg, r.item.origin))
                })
                .collect(),
        );
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

    #[test]
    fn the_summary_counts_every_outcome_and_a_failure_fails_the_run() {
        let results = vec![
            result("a", ItemOutcome::Updated, SrOutcome::Created("42".into())),
            result("b", ItemOutcome::Failed("build".into()), SrOutcome::NotEligible("failed".into())),
            result("c", ItemOutcome::Skipped("name".into()), SrOutcome::NotRequested),
            result("d", ItemOutcome::Updated, SrOutcome::Existing("7".into())),
        ];
        assert!(any_failed(&results));
        assert!(!any_failed(&results[..1]));
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
