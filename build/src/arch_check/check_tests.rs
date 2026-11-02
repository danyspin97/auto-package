//! The check run against a fake OBS: the reads, the one edit and the results.

use std::path::Path;
use std::time::Duration;

use super::check_fake::*;
use super::*;
use crate::obs_status::ResultEntry;

#[test]
fn by_default_the_check_reads_and_never_writes_the_branch() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |m: String| fake.log(format!("say:{m}"));
    let out = check_other_arches(&fake, &request(dir.path(), &say, false, false)).unwrap();
    assert!(fake.writes().is_empty(), "{:?}", fake.events.borrow());
    assert!(out.added.is_empty());
    // s390x is not in the branch, so there is nothing to check on it.
    assert!(out.rows.is_empty());
}

#[test]
fn the_guard_refuses_before_any_obs_read() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |_: String| {};
    let mut req = request(dir.path(), &say, true, false);
    req.branch = "devel:languages:rust";
    let err = check_other_arches(&fake, &req).unwrap_err();
    assert!(err.contains("not a home: project"), "{err}");
    assert!(
        fake.events.borrow().is_empty(),
        "{:?}",
        fake.events.borrow()
    );
}

#[test]
fn an_addition_is_announced_before_the_write_and_made_under_the_lock() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |m: String| fake.log(format!("say:{m}"));
    set_results(
        &fake,
        BRANCH,
        vec![entry("p", "standard", "s390x", "succeeded")],
    );
    let out = check_other_arches(&fake, &request(dir.path(), &say, true, false)).unwrap();
    assert_eq!(out.added.len(), 1);
    let events = fake.events.borrow().clone();
    let announce = events
        .iter()
        .position(|e| e.starts_with("say:adding arch s390x to repository standard"))
        .expect("the addition is announced");
    let write = events.iter().position(|e| e.starts_with("write:")).unwrap();
    assert!(announce < write, "{events:?}");
    assert!(
        events
            .iter()
            .any(|e| e == &format!("read(locked):{BRANCH}")),
        "{events:?}"
    );
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].state, ArchState::Succeeded);
}

#[test]
fn a_read_only_check_adds_nothing_even_when_asked() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |_: String| {};
    let out = check_other_arches(&fake, &request(dir.path(), &say, true, true)).unwrap();
    assert!(fake.writes().is_empty());
    assert!(out.added.is_empty());
    assert!(
        out.notes.iter().any(|n| n.contains("only read")),
        "{:?}",
        out.notes
    );
}

#[test]
fn a_failing_flavor_of_the_package_is_a_failing_row() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |_: String| {};
    with_branch_s390x(&fake);
    set_results(
        &fake,
        BRANCH,
        vec![
            entry("p", "standard", "s390x", "succeeded"),
            entry("p:python311", "standard", "s390x", "failed"),
        ],
    );
    set_results(
        &fake,
        ORIGIN,
        vec![entry("p", "standard", "s390x", "succeeded")],
    );
    fake.logs.borrow_mut().insert(
        ("p:python311".into(), "standard".into(), "s390x".into()),
        "[ 1s] x.c: error: no such header\n".into(),
    );
    let out = check_other_arches(&fake, &request(dir.path(), &say, false, false)).unwrap();
    assert_eq!(out.rows.len(), 1);
    let row = &out.rows[0];
    assert_eq!(row.pkg, "p:python311");
    assert!(row.state.is_failure(), "{row:?}");
    assert_eq!(
        row.state,
        ArchState::Failed {
            detail: Some("[ 1s] x.c: error: no such header".to_string())
        }
    );
    assert!(!row.baseline_fails);
}

#[test]
fn a_failure_the_origin_shares_is_marked_and_one_it_lacks_is_not() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |_: String| {};
    with_branch_s390x(&fake);
    set_results(
        &fake,
        BRANCH,
        vec![entry("p", "standard", "s390x", "failed")],
    );
    set_results(
        &fake,
        ORIGIN,
        vec![entry("p", "standard", "s390x", "failed")],
    );
    let out = check_other_arches(&fake, &request(dir.path(), &say, false, false)).unwrap();
    assert!(out.rows[0].baseline_fails);
    assert!(!out.rows[0].blocks_submit());

    set_results(
        &fake,
        ORIGIN,
        vec![entry("p", "standard", "s390x", "succeeded")],
    );
    let out = check_other_arches(&fake, &request(dir.path(), &say, false, false)).unwrap();
    assert!(!out.rows[0].baseline_fails);
    assert!(out.rows[0].blocks_submit());
}

#[test]
fn an_unreadable_origin_marks_nothing_and_says_so() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |_: String| {};
    with_branch_s390x(&fake);
    set_results(
        &fake,
        BRANCH,
        vec![entry("p", "standard", "s390x", "failed")],
    );
    let out = check_other_arches(&fake, &request(dir.path(), &say, false, false)).unwrap();
    assert!(!out.rows[0].baseline_fails);
    assert!(
        out.notes.iter().any(|n| n.contains("could not be read")),
        "{:?}",
        out.notes
    );
}

#[test]
fn pending_arches_are_reported_as_not_final() {
    let fake = fixture();
    let dir = tempfile::tempdir().unwrap();
    let say = |_: String| {};
    with_branch_s390x(&fake);
    set_results(
        &fake,
        BRANCH,
        vec![entry("p", "standard", "s390x", "scheduled")],
    );
    let out = check_other_arches(&fake, &request(dir.path(), &say, false, false)).unwrap();
    assert!(out.rows[0].state.is_pending());
    assert!(!out.rows[0].blocks_submit());
    assert!(
        out.notes.iter().any(|n| n.contains("not final")),
        "{:?}",
        out.notes
    );
}
