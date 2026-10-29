use super::*;

#[test]
fn a_package_the_pipeline_reported_built_is_settled_under_any_spelling() {
    assert!(!is_settled("python-settled-probe"));
    mark_settled("python-settled-probe");
    assert!(is_settled("python-settled-probe"));
    assert!(is_settled("python-Settled_Probe"));
    assert!(!is_settled("python-another-probe"));
}
