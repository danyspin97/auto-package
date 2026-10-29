use super::*;

#[test]
fn a_trailing_unpublished_marker_is_stripped_with_or_without_the_space() {
    assert_eq!(
        strip_result_suffixes("succeeded (unpublished)"),
        "succeeded"
    );
    assert_eq!(strip_result_suffixes("succeeded(unpublished)"), "succeeded");
    assert_eq!(strip_result_suffixes("succeeded*"), "succeeded");
    assert_eq!(
        strip_result_suffixes("succeeded * (unpublished)"),
        "succeeded"
    );
    assert_eq!(strip_result_suffixes("failed"), "failed");
    assert!(matches!(
        status_from_osc_text("succeeded (unpublished)"),
        BuildStatus::Succeeded
    ));
    assert!(matches!(
        status_from_osc_text("succeeded"),
        BuildStatus::Succeeded
    ));
}
