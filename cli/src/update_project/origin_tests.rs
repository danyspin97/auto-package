use super::*;

#[test]
fn a_readable_origin_is_not_skipped() {
    let read = Ok("Name: foo\nVersion: 1.0\n".to_string());
    assert_eq!(unreadable_origin(&read), None);
}

#[test]
fn an_origin_without_a_spec_is_skipped_with_that_reason() {
    let read = Err(update::ReadError::NoSpec);
    assert_eq!(unreadable_origin(&read), Some(update::SkipReason::NoSpec));
}

#[test]
fn a_broken_link_is_skipped_as_needing_the_maintainer() {
    let read = Err(update::ReadError::NeedsMaintainer(
        "the _link of foo cannot be expanded (conflict)".to_string(),
    ));
    assert_eq!(
        unreadable_origin(&read),
        Some(update::SkipReason::NeedsMaintainer(
            "the _link of foo cannot be expanded (conflict)".to_string()
        ))
    );
}

#[test]
fn a_read_that_failed_is_skipped_with_its_cause() {
    let read = Err(update::ReadError::Failed(
        "reading foo.spec (osc cat) failed: timeout, 3 attempts".to_string(),
    ));
    assert_eq!(
        unreadable_origin(&read),
        Some(update::SkipReason::ReadFailed(
            "reading foo.spec (osc cat) failed: timeout, 3 attempts".to_string()
        ))
    );
}

#[test]
fn only_a_failed_read_is_transient() {
    assert!(is_transient(&Err(update::ReadError::Failed(
        "timeout".into()
    ))));
    assert!(!is_transient(&Err(update::ReadError::NoSpec)));
    assert!(!is_transient(&Err(update::ReadError::NeedsMaintainer(
        "broken link".into()
    ))));
    assert!(!is_transient(&Ok("Name: foo\n".to_string())));
}
