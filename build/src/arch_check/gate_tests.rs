//! The submit gate's verdict, tested on rows.

use super::*;

fn failed() -> ArchState {
    ArchState::Failed { detail: None }
}

fn row(repo: &str, arch: &str, state: ArchState, baseline: Option<ArchState>) -> GateRow {
    GateRow {
        repo: repo.to_string(),
        arch: arch.to_string(),
        state,
        baseline: baseline.map(|state| Baseline {
            project: "origin".to_string(),
            state: Some(state),
        }),
    }
}

const PRIMARY: (&str, &str) = ("standard", "x86_64");

fn primary() -> GateRow {
    row("standard", "x86_64", ArchState::Succeeded, None)
}

#[test]
fn a_primary_that_does_not_build_refuses_even_when_allowed() {
    let rows = vec![row("standard", "x86_64", failed(), Some(failed()))];
    for allow in [false, true] {
        assert!(matches!(
            gate_verdict("p", PRIMARY, &rows, allow, true),
            GateVerdict::Refuse(_)
        ));
    }
}

#[test]
fn a_pending_arch_refuses_even_when_allowed() {
    let rows = vec![
        primary(),
        row(
            "ports",
            "s390x",
            ArchState::Pending("building".into()),
            None,
        ),
        row("ports", "ppc64le", failed(), Some(failed())),
    ];
    let GateVerdict::Refuse(why) = gate_verdict("p", PRIMARY, &rows, true, true) else {
        panic!("a pending arch must refuse");
    };
    assert!(why.contains("ports/s390x"), "{why}");
}

#[test]
fn a_failure_the_baseline_shares_passes_with_a_note() {
    let rows = vec![primary(), row("ports", "s390x", failed(), Some(failed()))];
    let GateVerdict::Pass { notes } = gate_verdict("p", PRIMARY, &rows, false, true) else {
        panic!("a shared failure must pass");
    };
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].contains("also fails in origin on ports/s390x"),
        "{notes:?}"
    );
}

#[test]
fn a_failure_the_baseline_does_not_share_refuses_unless_allowed() {
    let rows = vec![
        primary(),
        row("ports", "s390x", failed(), Some(ArchState::Succeeded)),
        row("ports", "ppc64le", failed(), Some(failed())),
    ];
    let GateVerdict::Refuse(why) = gate_verdict("p", PRIMARY, &rows, false, true) else {
        panic!("an unshared failure must refuse");
    };
    assert!(why.contains("ports/s390x"), "{why}");
    assert!(why.contains("--allow-arch-failures"), "{why}");
    assert!(matches!(
        gate_verdict("p", PRIMARY, &rows, true, true),
        GateVerdict::Pass { .. }
    ));
}

#[test]
fn a_failure_the_baseline_has_no_result_for_passes_with_a_note() {
    let no_result = GateRow {
        repo: "ports".to_string(),
        arch: "s390x".to_string(),
        state: failed(),
        baseline: Some(Baseline {
            project: "origin".to_string(),
            state: None,
        }),
    };
    let rows = vec![primary(), no_result];
    let GateVerdict::Pass { notes } = gate_verdict("p", PRIMARY, &rows, false, true) else {
        panic!("a failure the origin has no result for is a note, not a refusal");
    };
    assert!(
        notes
            .iter()
            .any(|n| n.contains("has no result") && n.contains("ports/s390x")),
        "{notes:?}"
    );
}

#[test]
fn a_failure_whose_baseline_was_not_read_refuses() {
    let unread = GateRow {
        repo: "ports".to_string(),
        arch: "s390x".to_string(),
        state: failed(),
        baseline: None,
    };
    let rows = vec![primary(), unread];
    let GateVerdict::Refuse(why) = gate_verdict("p", PRIMARY, &rows, false, true) else {
        panic!("a failure without a baseline is not known to be shared");
    };
    assert!(why.contains("ports/s390x"), "{why}");
}

#[test]
fn a_failure_the_origin_excludes_refuses() {
    let rows = vec![
        primary(),
        row("ports", "s390x", failed(), Some(ArchState::Ignored)),
    ];
    assert!(matches!(
        gate_verdict("p", PRIMARY, &rows, false, true),
        GateVerdict::Refuse(_)
    ));
}

#[test]
fn a_failure_status_that_no_result_explains_refuses() {
    let rows = vec![primary(), row("ports", "s390x", ArchState::Succeeded, None)];
    assert!(matches!(
        gate_verdict("p", PRIMARY, &rows, true, true),
        GateVerdict::Refuse(_)
    ));
}

#[test]
fn a_package_disabled_everywhere_else_passes_with_a_note_not_silently() {
    let rows = vec![
        primary(),
        row("ports", "s390x", ArchState::Disabled, None),
        row("ports", "ppc64le", ArchState::Disabled, None),
    ];
    let GateVerdict::Pass { notes } = gate_verdict("p", PRIMARY, &rows, false, false) else {
        panic!("a pass is expected: nothing fails");
    };
    assert_eq!(
        notes,
        [
            "p is not checked on the other architectures: every result there is disabled or excluded in the branch"
        ]
    );
}

#[test]
fn a_checked_arch_gives_no_not_checked_note() {
    let rows = vec![
        primary(),
        row("ports", "s390x", ArchState::Disabled, None),
        row("ports", "ppc64le", ArchState::Succeeded, None),
    ];
    let GateVerdict::Pass { notes } = gate_verdict("p", PRIMARY, &rows, false, false) else {
        panic!("a pass is expected");
    };
    assert!(notes.is_empty(), "{notes:?}");
}
