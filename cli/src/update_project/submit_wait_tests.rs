use super::*;
use std::time::Duration;

#[test]
fn a_submit_batch_waits_the_run_arch_wait_not_the_default() {
    let run = ArchDeadline::new(Duration::from_secs(5 * 60));
    let at = Instant::now();
    let batch = batch_deadline(&run, at);
    assert_eq!(batch.left(at), Duration::from_secs(5 * 60));
    assert_ne!(batch.left(at), osc::OVERALL_MAX_WAIT);
}

#[test]
fn the_run_deadline_is_the_bound_the_user_set() {
    let run = ArchDeadline::new(Duration::from_secs(45 * 60));
    assert_eq!(run.bound(), Duration::from_secs(45 * 60));
}

#[test]
fn a_submit_waits_the_run_deadline_once_a_build_ended() {
    let run = ArchDeadline::new(Duration::from_secs(5 * 60));
    let at = Instant::now();
    run.build_ended(at);
    let own = batch_deadline(&run, at);
    assert!(std::ptr::eq(submit_deadline(&run, &own), &run));
}
