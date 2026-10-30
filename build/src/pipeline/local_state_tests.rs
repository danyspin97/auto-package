use super::*;
use crate::pipeline::repeat_log::*;

#[test]
fn a_local_build_is_recorded_apart_from_an_obs_one() {
    let mut st = PkgState::default();
    st.record_built("1.2", true);
    assert_eq!(st.last_outcome.as_deref(), Some(LOCAL_SUCCESS));
    assert!(!st.is_built("1.2"));
    assert!(st.is_built_locally("1.2"));
    assert!(!st.is_built_locally("1.3"));

    st.record_built("1.2", false);
    assert!(st.is_built("1.2"));
    assert!(!st.is_built_locally("1.2"));
}

#[test]
fn a_local_build_clears_the_ladder_of_the_spec_it_built() {
    let mut st = PkgState {
        ladder_spec_hash: Some(7),
        ladder_reason: Some("r".into()),
        last_failure_reason: Some("old".into()),
        ..Default::default()
    };
    st.record_built("1.2", true);
    assert!(st.ladder_spec_hash.is_none() && st.ladder_reason.is_none());
    assert!(st.last_failure_reason.is_none());
}

#[test]
fn under_local_the_commit_step_does_not_run() {
    let mut ran = false;
    let r = commit_unless(true, || {
        ran = true;
        Ok(())
    });
    assert!(r.is_ok());
    assert!(!ran, "a local build must not commit to OBS");

    let mut ran = false;
    let r = commit_unless(false, || {
        ran = true;
        Ok(())
    });
    assert!(r.is_ok() && ran);
}

#[test]
fn under_local_a_repeated_build_log_is_read_not_rebuilt_on_obs() {
    assert_eq!(same_log_action(true, 1), SameLogAction::Continue);
    assert_eq!(same_log_action(false, 1), SameLogAction::AskObsRebuild);
    assert_eq!(same_log_action(true, 2), SameLogAction::Fail);
    assert_eq!(same_log_action(false, 2), SameLogAction::Fail);
}

#[test]
fn under_local_a_flaky_check_is_a_local_build() {
    assert_eq!(flaky_retry(true), FlakyRetry::LocalBuild);
    assert_eq!(flaky_retry(false), FlakyRetry::ObsRebuild);
}
