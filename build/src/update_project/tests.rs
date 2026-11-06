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
        upstream_tag: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: vec!["no-check: the spec has no %check".into()],
    };
    let d = sr_description(&i, None, &[], &[], &[]);
    assert!(d.starts_with("Improve libfoo 1.0"), "{d}");
    assert!(d.contains("- no-check: the spec has no %check"));
    assert!(!d.contains("was 1.0"));
    i.findings.clear();
    assert!(sr_description(&i, None, &[], &[], &[]).starts_with("Update libfoo to 1.0 (was 1.0)"));
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
            upstream_tag: None,
        },
        notes: Vec::new(),
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
        upstream_tag: None,
        dependency_of: None,
        requires: Vec::new(),
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
            notes: Vec::new(),
        },
        Candidate {
            rpm_name: "e".into(),
            decision: Decision::Skipped(SkipReason::NoSourceLine),
            notes: Vec::new(),
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
    let items = [needing_libbar(None), {
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
        maintainer: &[],
        submit: true,
    });
    assert_eq!(
        lines,
        [
            "dry run: nothing is branched, built, edited or submitted; Base:System stays untouched",
            "branch project: home:u:branches:Base:System (would be created by the first branch)",
            "packages to branch and update (2):",
            "  libfoo  1 -> 2  (from Base:System, requires the update of libbar)",
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
        maintainer: &[],
        submit: false,
    });
    assert!(lines.contains(&"branch project: b (exists)".to_string()));
    assert!(lines.contains(&"extra build paths: none needed".to_string()));
    assert!(lines.contains(&"submit requests: none (--submit not given)".to_string()));
}

/// The two skips that stop a package for the maintainer without a failure: a
/// branch ahead of upstream, and Repology disagreeing by a minor.
fn maintainer_lines() -> Vec<String> {
    vec![
        SkipReason::BranchAhead {
            branch: "2.0".into(),
            upstream: "1.5".into(),
        }
        .to_string(),
        SkipReason::RepologyDisagrees {
            source: "git".into(),
            target: "1.5".into(),
            repology: "1.6".into(),
        }
        .to_string(),
    ]
}

#[test]
fn the_dry_run_lists_what_needs_the_maintainer() {
    let maintainer: Vec<String> = maintainer_lines()
        .into_iter()
        .enumerate()
        .map(|(i, why)| format!("pkg{i}: {why}"))
        .collect();
    let lines = dry_run_lines(&DryRun {
        source: "S",
        branch_project: "b",
        branch_exists: true,
        items: &[],
        paths: &[],
        maintainer: &maintainer,
        submit: false,
    });
    let at = lines
        .iter()
        .position(|l| l == "needs the maintainer (2):")
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert_eq!(lines[at + 1], format!("  {}", maintainer[0]));
    assert_eq!(lines[at + 2], format!("  {}", maintainer[1]));
    assert!(lines[at + 1].contains("would replace it with an older version"));
    assert!(lines[at + 2].contains("a maintainer must pick the version"));
}

#[test]
fn the_dry_run_has_no_maintainer_heading_when_nothing_needs_one() {
    let lines = dry_run_lines(&DryRun {
        source: "S",
        branch_project: "b",
        branch_exists: true,
        items: &[],
        paths: &[],
        maintainer: &[],
        submit: false,
    });
    assert!(!lines.iter().any(|l| l.starts_with("needs the maintainer")));
}

#[test]
fn the_summary_lists_the_maintainer_skips_under_their_heading() {
    let maintainer = maintainer_lines();
    let text = summary_text("b", &[], &[], &[], &maintainer, false);
    assert!(text.contains("needs the maintainer (2):"), "{text}");
    for why in &maintainer {
        assert!(text.contains(&format!("  {why}")), "{text}");
    }
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
    let d = sr_description(&i, Some("- Update to 2.0"), &[], &[], &[]);
    assert!(d.starts_with("Update foo to 2.0 (was 1.0).\n\nChangelog:\n- Update to 2.0"));
    assert!(!sr_description(&i, None, &[], &[], &[]).contains("Changelog"));
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
    let d = sr_description(&i, Some("- Update to 2.0"), &flags, &[], &[]);
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
    assert!(!sr_description(&i, None, &[], &[], &[]).contains("Flagged"));
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
    let d = sr_description(&i, Some("- Update to 2.0"), &flags, &risks, &[]);
    assert!(
            d.contains(
                "\n\nFlagged for review:\n- FLAG: %check removed or left without a test: the tests do not run\n\nNeeds review:\n- RISK: new scriptlet: %post\n\nChangelog:\n"
            ),
            "{d}"
        );
    assert!(!sr_description(&i, None, &[], &[], &[]).contains("Needs review"));
}

#[test]
fn the_changelog_in_the_description_has_no_control_characters_and_is_capped() {
    let i = item("foo");
    let d = sr_description(&i, Some("- ok\x1b[31m red\r\n\x07bell\tTab"), &[], &[], &[]);
    assert!(d.contains("- ok[31m red\nbell Tab"), "{d:?}");
    assert!(!d.contains('\x1b') && !d.contains('\r') && !d.contains('\x07'));
    let long = "x".repeat(10_000);
    let d = sr_description(&i, Some(&long), &[], &[], &[]);
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
    let text = summary_text("home:u:b", &failed_sr, &[], &[], &[], true);
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
    assert!(!summary_text("home:u:b", &quiet, &[], &[], &[], true).contains("failed\n"));
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
    let text = summary_text(
        "home:u:b",
        &results,
        &[path("r", "devel:x")],
        &[],
        &[],
        true,
    );
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
    assert!(!summary_text("b", &results, &[], &[], &[], false).contains("submit requests"));
}

#[test]
fn a_broken_link_is_listed_under_needs_the_maintainer_and_is_no_failure() {
    let reason = "X11:Wayland/stacktile links to openSUSE:Factory/stacktile, deleted from \
                  openSUSE:Factory by request #1347021 (2026-04-17)."
        .to_string();
    let text = summary_text(
        "home:u:b",
        &[],
        &[],
        &[],
        std::slice::from_ref(&reason),
        false,
    );
    assert!(
        text.contains(&format!("needs the maintainer (1):\n  {reason}\n")),
        "{text}"
    );
    assert!(text.contains("failed (0):"), "{text}");
}

#[test]
fn the_already_requested_section_is_listed_only_when_there_is_one() {
    assert!(requested_lines(&[]).is_empty());
    assert_eq!(requested_section(&[]), "");
    let one = ["foo: submit request 7 (new, by u) already brings 2.0 to foo".to_string()];
    assert_eq!(
        requested_section(&one),
        "already requested (1):\n  foo: submit request 7 (new, by u) already brings 2.0 to foo\n"
    );
}

#[test]
fn a_request_says_per_architecture_what_is_not_final() {
    let i = item("foo");
    let not_final = vec![
        "scheduled on s390x at submit time".to_string(),
        "blocked on openSUSE_Factory_RISCV/riscv64 at submit time".to_string(),
    ];
    let d = sr_description(&i, None, &[], &[], &not_final);
    assert!(d.contains("Not final at submit time:"), "{d}");
    assert!(d.contains("- scheduled on s390x at submit time"), "{d}");
    assert!(
        d.contains("- blocked on openSUSE_Factory_RISCV/riscv64 at submit time"),
        "{d}"
    );
    assert!(!sr_description(&i, None, &[], &[], &[]).contains("Not final"));
}

/// libfoo, whose BuildRequires need libbar newer than its origin has.
fn needing_libbar(request: Option<&str>) -> Item {
    let mut i = item("libfoo");
    i.requires.push(RequiredUpdate {
        pkg: "libbar".into(),
        branch_package: "home:u:branches:Base:System/libbar".into(),
        requirement: "libbar-devel >= 2".into(),
        origin: "devel:languages:c_c++".into(),
        origin_version: "1".into(),
        request: request.map(str::to_string),
    });
    i
}

#[test]
fn a_request_names_the_dependency_update_it_requires() {
    let d = sr_description(&needing_libbar(Some("4242")), None, &[], &[], &[]);
    assert!(
        d.contains(
            "\n\nRequires the update of libbar (request #4242): the spec needs \
             libbar-devel >= 2; devel:languages:c_c++ has libbar 1.\n"
        ),
        "{d}"
    );
    let d = sr_description(&needing_libbar(None), None, &[], &[], &[]);
    assert!(
        d.contains(
            "Requires the update of libbar (branch package home:u:branches:Base:System/libbar)"
        ),
        "{d}"
    );
    assert!(!sr_description(&item("libfoo"), None, &[], &[], &[]).contains("Requires"));
}

#[test]
fn the_summary_names_the_dependency_update_a_package_requires() {
    let results = [ItemResult {
        item: needing_libbar(None),
        outcome: ItemOutcome::Updated,
        sr: SrOutcome::NotRequested,
    }];
    let text = summary_text("b", &results, &[], &[], &[], false);
    assert!(
        text.contains("  libfoo 1 -> 2 (requires the update of libbar)\n"),
        "{text}"
    );
}
