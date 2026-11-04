use super::build_check::{ReadOnlyArchObs, build_findings, build_report};
use super::fetch_fake::FakeRequestObs;
use super::gather::gather;
use super::{Finding, Severity, ids};
use crate::arch_check::{ArchObs, ArchState, Baseline, GateRow};
use crate::obs_status::ResultEntry;

fn row(repo: &str, arch: &str, state: ArchState, baseline: Option<ArchState>) -> GateRow {
    GateRow {
        repo: repo.into(),
        arch: arch.into(),
        baseline: baseline.map(|state| Baseline {
            project: "Base:System".into(),
            state: Some(state),
        }),
        state,
    }
}

fn failed() -> ArchState {
    ArchState::Failed {
        detail: Some("boom".into()),
    }
}

fn one(found: &[Finding]) -> (&'static str, Severity) {
    assert_eq!(found.len(), 1, "{found:?}");
    (found[0].id, found[0].severity)
}

#[test]
fn a_failure_the_baseline_does_not_have_is_a_blocker_and_one_it_has_is_a_note() {
    let rows = [row(
        "Tumbleweed",
        "x86_64",
        failed(),
        Some(ArchState::Succeeded),
    )];
    assert_eq!(
        one(&build_findings("p", &rows, &[], None)),
        (ids::BUILD_FAILED, Severity::Blocker)
    );
    let rows = [row("Tumbleweed", "x86_64", failed(), Some(failed()))];
    assert_eq!(
        one(&build_findings("p", &rows, &[], None)),
        (ids::BUILD_FAILS_IN_TARGET_TOO, Severity::Note)
    );
}

#[test]
fn a_failure_whose_baseline_was_not_read_is_a_blocker() {
    let rows = [GateRow {
        repo: "Tumbleweed".into(),
        arch: "riscv64".into(),
        state: failed(),
        baseline: None,
    }];
    assert_eq!(
        one(&build_findings("p", &rows, &[], None)),
        (ids::BUILD_FAILED, Severity::Blocker)
    );
}

#[test]
fn a_pending_result_is_a_warning_and_a_port_that_is_not_final_a_note() {
    let rows = [row(
        "Tumbleweed",
        "aarch64",
        ArchState::Pending("building".into()),
        None,
    )];
    let found = build_findings("p", &rows, &[], None);
    assert_eq!(one(&found), (ids::BUILD_PENDING, Severity::Warning));
    assert!(
        found[0]
            .message
            .contains("Tumbleweed/aarch64 is still building")
    );
    let rows = [row(
        "ports",
        "riscv64",
        ArchState::NotFinal("building".into()),
        None,
    )];
    assert_eq!(
        one(&build_findings("p", &rows, &[], None)),
        (ids::BUILD_PENDING, Severity::Note)
    );
    let rows = [row(
        "ports",
        "riscv64",
        ArchState::NotTestable { detail: None },
        None,
    )];
    assert_eq!(
        one(&build_findings("p", &rows, &[], None)),
        (ids::BUILD_NOT_TESTABLE, Severity::Note)
    );
}

#[test]
fn no_rows_is_no_results_and_a_moved_source_is_stale() {
    assert_eq!(
        one(&build_findings("p", &[], &[], None)),
        (ids::BUILD_NO_RESULTS, Severity::Warning)
    );
    let rows = [row("T", "x86_64", ArchState::Succeeded, None)];
    assert_eq!(
        one(&build_findings("p", &rows, &[], Some(true))),
        (ids::BUILD_STALE, Severity::Warning)
    );
    assert!(build_findings("p", &rows, &[], Some(false)).is_empty());
}

#[test]
fn an_arch_the_source_disables_and_the_target_builds_is_a_note() {
    let rows = [row("T", "s390x", ArchState::Disabled, None)];
    let target = [(("T".to_string(), "s390x".to_string()), ArchState::Succeeded)];
    assert_eq!(
        one(&build_findings("p", &rows, &target, None)),
        (ids::BUILD_DISABLED_ARCH, Severity::Note)
    );
    let target = [(("T".to_string(), "s390x".to_string()), ArchState::Disabled)];
    assert!(build_findings("p", &rows, &target, None).is_empty());
}

#[test]
fn the_adapter_refuses_every_write() {
    let obs = FakeRequestObs::default();
    let a = ReadOnlyArchObs(&obs);
    assert!(a.set_project_meta("p", "<project/>").is_err());
    assert!(a.enable_repositories("p", "k", &["r"]).is_err());
    assert!(
        a.buildlog("p", "k", "r", "a", std::path::Path::new("/nonexistent/x"))
            .is_err()
    );
    assert!(obs.calls.borrow().is_empty());
}

fn entry(repo: &str, arch: &str, code: &str) -> ResultEntry {
    ResultEntry {
        repo: repo.into(),
        arch: arch.into(),
        dirty: false,
        pkg: "cracklib".into(),
        code: code.into(),
        details: None,
    }
}

#[test]
fn the_report_reads_source_and_baseline_results_through_the_fake() {
    let mut o = fake_with_request();
    let sp = "home:dspinella:auto-package:branches:Base:System";
    o.results.insert(
        (sp.into(), "cracklib".into()),
        vec![
            entry("T", "x86_64", "succeeded"),
            entry("T", "aarch64", "failed"),
        ],
    );
    o.results.insert(
        ("Base:System".into(), "cracklib".into()),
        vec![
            entry("T", "x86_64", "succeeded"),
            entry("T", "aarch64", "succeeded"),
        ],
    );
    let inputs = gather(&o, "1394662").unwrap();
    let found = build_report(&o, &inputs.actions[0]).unwrap();
    assert_eq!(one(&found), (ids::BUILD_FAILED, Severity::Blocker));
    assert!(found[0].message.contains("T/aarch64"));
    // Without results anywhere: no results.
    o.results.clear();
    let found = build_report(&o, &inputs.actions[0]).unwrap();
    assert_eq!(one(&found), (ids::BUILD_NO_RESULTS, Severity::Warning));
}

#[test]
fn a_failed_read_of_the_source_or_the_target_results_is_an_error() {
    let mut o = fake_with_request();
    o.failing
        .insert("results", super::fetch::ReadFailure::new("503"));
    let inputs = gather(&o, "1394662").unwrap();
    let e = build_report(&o, &inputs.actions[0]).unwrap_err();
    assert!(e.contains("could not be read") && e.contains("503"), "{e}");
}

/// The fake holding request 1394662, as the gather tests build it.
fn fake_with_request() -> FakeRequestObs {
    super::gather_tests::obs()
}
