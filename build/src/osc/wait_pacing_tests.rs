use super::*;
use crate::osc::poll::*;

const SECS: fn(u64) -> Duration = Duration::from_secs;

#[test]
fn a_stale_status_is_checked_after_thirty_seconds_then_every_minute() {
    assert!(!recheck_due(true, SECS(29), None));
    assert!(recheck_due(true, SECS(30), None));
    assert!(!recheck_due(true, SECS(200), Some(SECS(59))));
    assert!(recheck_due(true, SECS(200), Some(SECS(60))));
}

#[test]
fn any_other_unchanged_status_is_checked_once_after_five_minutes() {
    assert!(!recheck_due(false, SECS(299), None));
    assert!(recheck_due(false, SECS(300), None));
    assert!(!recheck_due(false, SECS(900), Some(SECS(600))));
}

#[test]
fn a_status_awaiting_the_new_revision_never_backs_off_past_thirty_seconds() {
    assert_eq!(wait_polls(40, true), 7);
    assert_eq!(poll_delay(wait_polls(40, true), 0, 0.5), SECS(30));
    assert_eq!(wait_polls(2, true), 2);
    assert_eq!(poll_delay(wait_polls(40, false), 0, 0.5), SECS(60));
}
