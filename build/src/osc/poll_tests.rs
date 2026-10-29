
use super::*;

#[test]
fn the_delay_grows_while_the_status_holds_still() {
    let d = |n| poll_delay(n, 0, 0.5).as_secs();
    assert_eq!(
        (d(0), d(3), d(4), d(7), d(8), d(100)),
        (15, 15, 30, 30, 60, 60)
    );
}

#[test]
fn failing_polls_back_off_to_two_minutes() {
    let d = |e| poll_delay(0, e, 0.5).as_secs();
    assert_eq!((d(1), d(2), d(3), d(8)), (30, 60, 120, 120));
    assert_eq!(poll_delay(8, 5, 0.5).as_secs(), 120);
}

#[test]
fn jitter_stays_within_a_fifth() {
    assert_eq!(poll_delay(0, 0, 0.0).as_secs_f64(), 12.0);
    assert!(poll_delay(0, 0, 0.999).as_secs_f64() < 18.0);
}

#[test]
fn the_http_status_is_read_from_either_osc_wording() {
    assert_eq!(http_status("HTTP Error 429: Too Many Requests"), Some(429));
    assert_eq!(
        http_status("x\n503 Server Error: Service Unavailable for url"),
        Some(503)
    );
    assert_eq!(http_status("401 Client Error: Unauthorized"), Some(401));
    assert_eq!(http_status("connection reset by peer"), None);
    assert_eq!(http_status("took 5000 Server Error"), None);
}
