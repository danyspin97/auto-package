//! `abort_run` flips process-wide state, so it lives in its own test binary.

use ap_build::proc::{abort_reason, abort_run, shutting_down};

#[test]
fn first_abort_reason_wins_and_stops_the_run() {
    assert_eq!(abort_reason(), None);
    assert!(!shutting_down());
    abort_run("first".to_string());
    abort_run("second".to_string());
    assert_eq!(abort_reason().as_deref(), Some("first"));
    assert!(shutting_down());
}
