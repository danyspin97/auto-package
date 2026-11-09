//! The bounds on a wait (see `wait_patience`): a wait is kept while it is not
//! exhausted, exhausted by its count or its age, reset by progress, and a
//! record written before the counters existed reads as fresh.

use super::wait_patience::*;

const NOW: u64 = 1_794_300_000;
const HOUR: u64 = 3600;

#[test]
fn a_young_wait_with_few_consultations_keeps_waiting() {
    assert_eq!(exhausted(1, NOW - HOUR, NOW), None);
    assert_eq!(exhausted(MAX_CONSULTATIONS - 1, NOW - 5 * HOUR, NOW), None);
}

#[test]
fn a_wait_is_exhausted_by_its_count() {
    assert_eq!(
        exhausted(MAX_CONSULTATIONS, NOW - HOUR, NOW),
        Some(Exhausted::Consultations(MAX_CONSULTATIONS))
    );
}

#[test]
fn a_wait_is_exhausted_by_its_age() {
    assert_eq!(
        exhausted(2, NOW - MAX_WAIT_SECS, NOW),
        Some(Exhausted::Age(MAX_WAIT_SECS))
    );
    assert_eq!(exhausted(2, NOW - MAX_WAIT_SECS + 1, NOW), None);
}

#[test]
fn a_record_without_a_creation_time_is_not_exhausted_by_age() {
    assert_eq!(exhausted(0, 0, NOW), None);
    assert_eq!(exhausted(1, 0, u64::MAX), None);
}

#[test]
fn no_progress_keeps_the_counters_going_on() {
    assert_eq!(carried(Some((7, NOW - HOUR)), true, NOW), (7, NOW - HOUR));
}

#[test]
fn progress_starts_the_counters_again() {
    assert_eq!(carried(Some((7, NOW - HOUR)), false, NOW), (0, NOW));
    assert_eq!(carried(None, false, NOW), (0, NOW));
    assert_eq!(carried(None, true, NOW), (0, NOW));
}

#[test]
fn an_old_record_without_the_fields_reads_as_fresh() {
    // The record of python-pyinstaller (no `consulted`, no `since`).
    let old =
        r#"{"spec":1,"rpms":5,"wait":{"kind":"missing","deps":["x"],"native":[],"outdated":[]}}"#;
    let record: super::wait_record::WaitRecord = serde_json::from_str(old).unwrap();
    assert_eq!((record.consulted, record.since), (0, 0));
    // Its first consultation makes it made now, not exhausted by age.
    let (consulted, since) = carried(Some((record.consulted, record.since)), true, NOW);
    assert_eq!((consulted, since), (0, NOW));
    assert_eq!(exhausted(consulted + 1, since, NOW), None);
}

#[test]
fn the_message_names_the_count_the_bound_and_the_version_held() {
    let msg = needs_human_message(
        "pyinstaller-hooks-contrib",
        Exhausted::Consultations(20),
        Some("2026.7"),
        Some("2026.5"),
        false,
    );
    assert!(
        msg.starts_with(
            "needs a human: waited 20 times for pyinstaller-hooks-contrib >= 2026.7 \
             and the repository still has 2026.5"
        ),
        "{msg}"
    );
    assert!(msg.contains("Next step:"), "{msg}");
    assert!(!msg.contains("built in this run"), "{msg}");
}

#[test]
fn the_message_says_the_built_dependency_is_not_in_the_branch_copy() {
    let msg = needs_human_message(
        "flit-core",
        Exhausted::Age(6 * HOUR),
        Some("3.9"),
        Some("3.8"),
        true,
    );
    assert!(msg.contains("waited 6 hours"), "{msg}");
    assert!(
        msg.contains(
            "flit-core was built in this run but its commit is not in the branch copy yet"
        ),
        "{msg}"
    );
}

#[test]
fn a_wait_without_a_bound_or_an_unreadable_copy_says_so() {
    let msg = needs_human_message("foo", Exhausted::Consultations(20), None, None, false);
    assert!(msg.contains("for foo, which is not built here"), "{msg}");
    let msg = needs_human_message("foo", Exhausted::Consultations(20), Some("2"), None, false);
    assert!(msg.contains("cannot be read"), "{msg}");
}
