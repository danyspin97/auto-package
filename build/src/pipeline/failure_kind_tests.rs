//! What each consumer of `FailureKind` does with `Environment`: the same as
//! with `Other`, except where `failure_kind.rs` says otherwise.

use super::*;

fn failed(kind: FailureKind) -> PipelineResult {
    PipelineResult::Failed {
        rpm_name: "pkg".into(),
        reason: "why".into(),
        kind,
    }
}

#[test]
fn an_environment_failure_never_ends_the_package_as_a_turn_does() {
    assert!(!agent::ends_package(FailureKind::Environment));
    assert_eq!(
        agent::ends_package(FailureKind::Environment),
        agent::ends_package(FailureKind::Other)
    );
}

#[test]
fn an_aborted_run_reports_an_environment_failure_as_the_abort() {
    assert!(reported_as_abort(FailureKind::Environment));
    assert!(reported_as_abort(FailureKind::Other));
    // Kinds an abort cannot have caused keep their own reason.
    for kind in [
        FailureKind::NeedsMaintainer,
        FailureKind::Excluded,
        FailureKind::Busy,
        FailureKind::Exhausted,
        FailureKind::RateLimited,
    ] {
        assert!(!reported_as_abort(kind), "{kind:?}");
    }
}

#[test]
fn a_failed_fix_or_findings_turn_says_failed_for_an_environment_failure() {
    assert_eq!(turn_failure_verdict(FailureKind::Environment), "failed");
    assert_eq!(
        turn_failure_verdict(FailureKind::Environment),
        turn_failure_verdict(FailureKind::Other)
    );
}

#[test]
fn an_environment_failure_does_not_decline_a_patch_request() {
    assert!(!declines_request_after_ladder(FailureKind::Environment));
    assert!(!declines_request_after_turn(FailureKind::Environment));
    // The package-side kinds still do.
    assert!(declines_request_after_ladder(FailureKind::Other));
    assert!(declines_request_after_turn(FailureKind::Other));
}

#[test]
fn an_environment_failure_fails_a_spec_style_fix_like_other_does() {
    assert!(!style_failure_warns(FailureKind::Environment));
    assert_eq!(
        style_failure_warns(FailureKind::Environment),
        style_failure_warns(FailureKind::Other)
    );
    assert!(style_failure_warns(FailureKind::Transient));
}

#[test]
fn an_environment_failure_is_recorded_as_a_failed_package() {
    assert_eq!(
        commit::recorded_outcome(&failed(FailureKind::Environment)),
        Some(("failed", "why".to_string()))
    );
    assert_eq!(
        commit::recorded_outcome(&failed(FailureKind::Environment)),
        commit::recorded_outcome(&failed(FailureKind::Other))
    );
}
