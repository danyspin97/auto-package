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
            bounds: Vec::new(),
        },
        consulted: 0,
        since: 0,
    }
}

/// python-tomlrt, after its first MissingDeps on flit-core: every later outer
/// run began with a Build step that ended in the same UnresolvableDeps
/// (python313-flit-core, python314-flit-core) and the same MissingDeps.
#[test]
fn a_package_waiting_on_an_unbuilt_dependency_is_not_tried_again() {
    let dir = tempfile::tempdir().unwrap();
    let record = missing(&["flit-core"]);
    assert_eq!(
        waiting_on(Some(&record), 1, &[dir.path()], &|_, _| true),
        Some(vec!["flit-core".to_string()])
    );
}

#[test]
fn a_built_dependency_or_a_changed_spec_needs_a_try() {
    let dir = tempfile::tempdir().unwrap();
    let record = missing(&["flit-core"]);
    // python-flit-core finished: its RPM is here now, under its flavour.
    std::fs::write(
        dir.path().join("python313-flit-core-3.9-1.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    assert_eq!(
        waiting_on(Some(&record), 1, &[dir.path()], &|_, _| true),
        None
    );
    // The spec was edited since the answer: the new spec is built.
    assert_eq!(waiting_on(Some(&record), 2, &[], &|_, _| true), None);
    assert_eq!(waiting_on(None, 1, &[], &|_, _| true), None);
    assert_eq!(waiting_on(Some(&missing(&[])), 1, &[], &|_, _| true), None);
}

/// The record of python-pyinstaller in run 20261109-173516: it waited on
/// pyinstaller-hooks-contrib at 2026.7, and was never tried again although
/// the RPM was built (python313-, python314-pyinstaller-hooks-contrib-2026.7).
const PYINSTALLER_RECORD: &str = r#"{"spec":17521263803251063915,"rpms":11308742877575830032,"wait":{"kind":"missing","deps":["pyinstaller-hooks-contrib"],"native":[],"outdated":[["pyinstaller-hooks-contrib","2026.7"]]}}"#;

#[test]
fn the_pyinstaller_wait_ends_once_its_dependency_is_built() {
    let record: WaitRecord = serde_json::from_str(PYINSTALLER_RECORD).unwrap();
    let spec = record.spec;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path()
            .join("python313-pyinstaller-hooks-contrib-2026.6-32.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    let dirs = [dir.path()];
    // An older release does not meet the 2026.7 bound: still waiting.
    assert_eq!(
        waiting_on(Some(&record), spec, &dirs, &|_, _| true),
        Some(vec!["pyinstaller-hooks-contrib".to_string()])
    );
    std::fs::write(
        dir.path()
            .join("python313-pyinstaller-hooks-contrib-2026.7-32.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    std::fs::write(
        dir.path()
            .join("python314-pyinstaller-hooks-contrib-2026.7-32.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    assert_eq!(waiting_on(Some(&record), spec, &dirs, &|_, _| true), None);
}

#[test]
fn a_local_retry_is_not_repeated_with_the_same_spec_and_rpms() {
    let retried = WaitRecord {
        spec: 1,
        rpms: 5,
        wait: Wait::LocalRetry,
        consulted: 0,
        since: 0,
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
            bounds: vec![("flit-core".into(), "3.9".into())],
        },
        consulted: 3,
        since: 1_794_000_000,
    };
    let text = serde_json::to_string(&record).expect("serialise");
    let back: WaitRecord = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, record);
}

/// python-pyinstaller in run 20261109-193719: the record came from a round
/// whose OBS report named the bare dependency; the `>= 2026.7` bound is the
/// spec's own (the translated turn raised it), so `outdated` is empty.
const PYINSTALLER_SPEC_BOUND_RECORD: &str = r#"{"spec":1,"rpms":5,"wait":{"kind":"missing","deps":["pyinstaller-hooks-contrib"],"native":[],"outdated":[],"bounds":[["pyinstaller-hooks-contrib","2026.7"]]}}"#;

#[test]
fn a_local_rpm_that_meets_the_spec_bound_is_not_enough_when_the_copy_is_older() {
    let record: WaitRecord = serde_json::from_str(PYINSTALLER_SPEC_BOUND_RECORD).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path()
            .join("python313-pyinstaller-hooks-contrib-2026.7-32.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    let dirs = [dir.path()];
    // The repository's copy is 2026.6 (not built on OBS yet): still waiting.
    let older = |_: &str, bound: &str| !is_newer_for_test(bound, "2026.6");
    assert_eq!(
        waiting_on(Some(&record), 1, &dirs, &older),
        Some(vec!["pyinstaller-hooks-contrib".to_string()])
    );
    // The copy meets the bound: the wait is over, the try builds.
    assert_eq!(waiting_on(Some(&record), 1, &dirs, &|_, _| true), None);
}

#[test]
fn a_local_rpm_below_the_spec_bound_keeps_waiting_whatever_the_copy() {
    let record: WaitRecord = serde_json::from_str(PYINSTALLER_SPEC_BOUND_RECORD).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path()
            .join("python313-pyinstaller-hooks-contrib-2026.6-32.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    assert_eq!(
        waiting_on(Some(&record), 1, &[dir.path()], &|_, _| true),
        Some(vec!["pyinstaller-hooks-contrib".to_string()])
    );
}

/// A record written before bounds were kept has none: the RPM alone decides,
/// as it did.
#[test]
fn a_record_without_bounds_reads_as_before() {
    let text = r#"{"spec":1,"rpms":5,"wait":{"kind":"missing","deps":["flit-core"],"native":[],"outdated":[]}}"#;
    let record: WaitRecord = serde_json::from_str(text).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("python313-flit-core-3.9-1.1.noarch.rpm"),
        b"",
    )
    .unwrap();
    assert_eq!(
        waiting_on(Some(&record), 1, &[dir.path()], &|_, _| false),
        None
    );
}

/// The test's stand-in for "the copy `have` meets `bound`".
fn is_newer_for_test(bound: &str, have: &str) -> bool {
    crate::update::is_newer(bound, have)
}
