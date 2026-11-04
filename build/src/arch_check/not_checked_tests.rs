//! The not-checked lines of the summary: a package whose other results were all skipped.

use super::*;

fn row(repo: &str, arch: &str, state: ArchState) -> ArchRow {
    ArchRow {
        pkg: "debugedit".into(),
        repo: repo.into(),
        arch: arch.into(),
        state,
        baseline: BaselineState::Builds,
        log: None,
    }
}

#[test]
fn disabled_results_are_named_as_disabled_not_excluded() {
    assert_eq!(ArchState::Disabled.label(), "disabled");
    assert!(ArchState::Disabled.is_skipped());
    assert!(!ArchState::Disabled.is_failure());
}

#[test]
fn a_check_where_nothing_was_built_says_so_in_the_summary() {
    let rows = vec![
        row("ports", "s390x", ArchState::Disabled),
        row("ports", "ppc64le", ArchState::Disabled),
        row("standard", "aarch64", ArchState::Ignored),
    ];
    assert_eq!(
        section_lines(&rows),
        [
            "debugedit: not checked on the other architectures: every result is disabled \
             or excluded in the branch (3 results)"
        ]
    );
    let disabled_only = &rows[..2];
    assert_eq!(
        section_lines(disabled_only),
        [
            "debugedit: not checked on the other architectures: the repositories are \
             disabled in the branch (2 results)"
        ]
    );
}

#[test]
fn a_built_result_beside_disabled_ones_is_not_a_not_checked_line() {
    let rows = vec![
        row("ports", "s390x", ArchState::Disabled),
        row("ports", "ppc64le", ArchState::Succeeded),
    ];
    assert!(section_lines(&rows).is_empty());
}
