//! A run stopped before it branched a package: the packages found and not
//! started are listed, and are not failures of the package.

use super::*;
use crate::update::Ecosystem;

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
        held_version: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: Vec::new(),
    }
}

fn result(pkg: &str, outcome: ItemOutcome, sr: SrOutcome) -> ItemResult {
    ItemResult {
        item: item(pkg),
        outcome,
        sr,
    }
}

#[test]
fn a_run_stopped_before_branching_lists_the_packages_not_started_and_fails_it() {
    let results = vec![
        result(
            "a",
            ItemOutcome::Failed("build".into()),
            SrOutcome::NotRequested,
        ),
        result("b", ItemOutcome::NotStarted, SrOutcome::NotRequested),
        result("c", ItemOutcome::NotStarted, SrOutcome::NotRequested),
    ];
    // Not started is not a failure of the package: only `a` is counted as one.
    assert_eq!(
        failure_lines(&results),
        [
            "1 package(s) failed to update",
            "2 package(s) not started: the run was interrupted"
        ]
    );
    let text = summary_text("home:u:b", &results, &[], &[], &[], false, 100);
    assert!(
        text.contains("Not finished (the run was interrupted) (2):\n  b\n  c\n"),
        "{text}"
    );
    assert!(text.contains("Failed (1):\n  a: build\n"), "{text}");
    // An interrupted run that failed nothing still fails: the exit decision.
    let only_stopped = &results[1..2];
    assert!(any_failed(only_stopped));
    assert_eq!(
        failure_lines(only_stopped),
        ["1 package(s) not started: the run was interrupted"]
    );
    assert!(!any_failed(&results[..0]));
    assert!(sr_eligibility(&item("b"), &ItemOutcome::NotStarted, None, None).is_err());
}

#[test]
fn an_interrupt_is_one_line_counting_what_it_left_unfinished() {
    let results = vec![
        result("a", ItemOutcome::Interrupted, SrOutcome::NotRequested),
        result("b", ItemOutcome::NotStarted, SrOutcome::NotRequested),
        result("c", ItemOutcome::NotStarted, SrOutcome::NotRequested),
        result("d", ItemOutcome::Updated, SrOutcome::NotRequested),
    ];
    assert_eq!(
        interrupt_summary(&results).as_deref(),
        Some(
            "interrupted: 2 package(s) not started, 1 in flight stopped; \
             re-run the same command to continue"
        )
    );
    // Nothing left unfinished, nothing to say.
    assert_eq!(interrupt_summary(&results[3..]), None);
    // A package stopped in flight is not a failure of the package.
    assert!(!any_failed(&results[..1]));
    assert!(sr_eligibility(&item("a"), &ItemOutcome::Interrupted, None, None).is_err());
    let text = summary_text("home:u:b", &results, &[], &[], &[], false, 100);
    assert!(
        text.contains("Not finished (the run was interrupted) (3):\n  a\n  b\n  c\n"),
        "{text}"
    );
}
