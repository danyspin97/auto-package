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
            state,
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
            gate_verdict("p", PRIMARY, &rows, allow),
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
    let GateVerdict::Refuse(why) = gate_verdict("p", PRIMARY, &rows, true) else {
        panic!("a pending arch must refuse");
    };
    assert!(why.contains("ports/s390x"), "{why}");
}

#[test]
fn a_failure_the_baseline_shares_passes_with_a_note() {
    let rows = vec![primary(), row("ports", "s390x", failed(), Some(failed()))];
    let GateVerdict::Pass { notes } = gate_verdict("p", PRIMARY, &rows, false) else {
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
    let GateVerdict::Refuse(why) = gate_verdict("p", PRIMARY, &rows, false) else {
        panic!("an unshared failure must refuse");
    };
    assert!(why.contains("ports/s390x"), "{why}");
    assert!(why.contains("--allow-arch-failures"), "{why}");
    assert!(matches!(
        gate_verdict("p", PRIMARY, &rows, true),
        GateVerdict::Pass { .. }
    ));
}

#[test]
fn a_failure_status_that_no_result_explains_refuses() {
    let rows = vec![primary(), row("ports", "s390x", ArchState::Succeeded, None)];
    assert!(matches!(
        gate_verdict("p", PRIMARY, &rows, true),
        GateVerdict::Refuse(_)
    ));
}
