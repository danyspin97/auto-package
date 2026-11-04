use super::*;

#[test]
fn a_finished_build_without_a_result_is_still_in_flight() {
    let finished = osc::BuildStatus::Unknown("finished".to_string());
    assert!(status_is_resolvable(Some(&finished)));
    assert!(!status_is_resolvable(Some(&osc::BuildStatus::Failed)));
    assert!(!status_is_resolvable(Some(&osc::BuildStatus::Unknown(
        "weird".into()
    ))));
}
