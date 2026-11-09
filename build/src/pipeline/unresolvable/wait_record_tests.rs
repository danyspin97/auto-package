//! The answers a recorded round gives to the next try, checked on the cases
//! of two real runs (`run.jsonl`: python-tomlrt in run 20261109-133411 and
//! aws-c-http in run 20261027-175210).

use super::wait_record::*;

fn missing(deps: &[&str]) -> WaitRecord {
    WaitRecord {
        spec: 1,
        rpms: 5,
        wait: Wait::Missing {
            deps: deps.iter().map(|d| d.to_string()).collect(),
            native: Vec::new(),
            outdated: Vec::new(),
        },
    }
}

/// python-tomlrt, after its first MissingDeps on flit-core: every later outer
/// run began with a Build step that ended in the same UnresolvableDeps
/// (python313-flit-core, python314-flit-core) and the same MissingDeps.
#[test]
fn a_package_waiting_on_an_unbuilt_dependency_is_not_tried_again() {
    let record = missing(&["flit-core"]);
    let nothing_built = |_: &str| false;
    assert_eq!(
        waiting_on(Some(&record), 1, nothing_built),
        Some(vec!["flit-core".to_string()])
    );
}

#[test]
fn a_built_dependency_or_a_changed_spec_needs_a_try() {
    let record = missing(&["flit-core"]);
    // python-flit-core finished: its RPM is here now.
    assert_eq!(waiting_on(Some(&record), 1, |d| d == "flit-core"), None);
    // The spec was edited since the answer: the new spec is built.
    assert_eq!(waiting_on(Some(&record), 2, |_| false), None);
    assert_eq!(waiting_on(None, 1, |_| false), None);
    assert_eq!(waiting_on(Some(&missing(&[])), 1, |_| false), None);
}

#[test]
fn a_local_retry_is_not_repeated_with_the_same_spec_and_rpms() {
    let retried = WaitRecord {
        spec: 1,
        rpms: 5,
        wait: Wait::LocalRetry,
    };
    // aws-c-http: the retry after its first answer saw the same spec and RPMs.
    assert!(local_retry_repeats(Some(&retried), 1, 5));
    // A new RPM (another package built) or a new spec is a change.
    assert!(!local_retry_repeats(Some(&retried), 1, 6));
    assert!(!local_retry_repeats(Some(&retried), 2, 5));
    // A missing-deps answer is not a retry.
    assert!(!local_retry_repeats(Some(&missing(&["x"])), 1, 5));
    assert!(!local_retry_repeats(None, 1, 5));
}

#[test]
fn the_record_survives_a_round_trip_through_json() {
    let record = WaitRecord {
        spec: 7,
        rpms: 9,
        wait: Wait::Missing {
            deps: vec!["flit-core".into()],
            native: vec!["libfoo".into()],
            outdated: vec![("bar".into(), Some("2.0".into())), ("baz".into(), None)],
        },
    };
    let text = serde_json::to_string(&record).expect("serialise");
    let back: WaitRecord = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, record);
}
