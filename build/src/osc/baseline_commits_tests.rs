use super::*;

/// A checkout whose `.osc/_files` records `rev`.
fn checkout_at(rev: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".osc")).unwrap();
    fs::write(
        dir.path().join(".osc/_files"),
        format!(r#"<directory name="foo" rev="1" srcmd5="{rev}"></directory>"#),
    )
    .unwrap();
    dir
}

/// What osc does when a commit lands: it records the new revision.
fn land(checkout: &Path, rev: &str) {
    fs::write(
        checkout.join(".osc/_files"),
        format!(r#"<directory name="foo" rev="2" srcmd5="{rev}"></directory>"#),
    )
    .unwrap();
}

#[test]
fn the_marker_is_set_during_the_commit_and_the_landed_revision_recorded_after() {
    let co = checkout_at("aaa");
    let base = tempfile::tempdir().unwrap();
    let res = record_commit(base.path(), co.path(), || {
        assert_eq!(pending_commit_base(base.path()).as_deref(), Some("aaa"));
        land(co.path(), "bbb");
        Ok(())
    });
    assert!(res.is_ok());
    assert_eq!(committed_revisions(base.path()), vec!["bbb".to_string()]);
    assert_eq!(pending_commit_base(base.path()), None);
}

#[test]
fn a_commit_cut_short_leaves_its_marker_and_records_nothing() {
    let co = checkout_at("aaa");
    let base = tempfile::tempdir().unwrap();
    let res = record_commit(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    assert!(res.is_err());
    assert_eq!(pending_commit_base(base.path()).as_deref(), Some("aaa"));
    assert!(committed_revisions(base.path()).is_empty());
}

#[test]
fn each_commit_of_an_attempt_adds_its_revision_in_order() {
    let co = checkout_at("aaa");
    let base = tempfile::tempdir().unwrap();
    record_commit(base.path(), co.path(), || {
        land(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    record_commit(base.path(), co.path(), || {
        land(co.path(), "ccc");
        Ok(())
    })
    .unwrap();
    assert_eq!(
        committed_revisions(base.path()),
        vec!["bbb".to_string(), "ccc".to_string()]
    );
}

#[test]
fn a_commit_that_left_the_revision_unchanged_records_nothing() {
    let co = checkout_at("aaa");
    let base = tempfile::tempdir().unwrap();
    record_commit(base.path(), co.path(), || Ok(())).unwrap();
    assert!(committed_revisions(base.path()).is_empty());
    assert_eq!(pending_commit_base(base.path()).as_deref(), Some("aaa"));
}

#[test]
fn a_package_with_a_service_file_records_nothing() {
    let co = checkout_at("aaa");
    fs::write(co.path().join("_service"), "<services/>").unwrap();
    let base = tempfile::tempdir().unwrap();
    record_commit(base.path(), co.path(), || {
        land(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    assert!(committed_revisions(base.path()).is_empty());
    assert_eq!(pending_commit_base(base.path()).as_deref(), Some("aaa"));
}

#[test]
fn a_service_listed_in_the_listing_but_not_checked_out_records_nothing() {
    let co = checkout_at("aaa");
    fs::write(
        co.path().join(".osc/_files"),
        r#"<directory name="foo" srcmd5="aaa"><entry name="_service" md5="x"/></directory>"#,
    )
    .unwrap();
    let base = tempfile::tempdir().unwrap();
    record_commit(base.path(), co.path(), || {
        fs::write(
            co.path().join(".osc/_files"),
            r#"<directory name="foo" srcmd5="bbb"><entry name="_service" md5="x"/></directory>"#,
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    assert!(committed_revisions(base.path()).is_empty());
}

#[test]
fn a_restore_lands_in_its_own_log_not_the_attempts() {
    let co = checkout_at("aaa");
    let base = tempfile::tempdir().unwrap();
    record_restore(
        base.path(),
        co.path(),
        || {
            land(co.path(), "bbb");
            Ok(())
        },
        || Err("no OBS".to_string()),
    )
    .unwrap();
    assert_eq!(restored_revisions(base.path()), vec!["bbb".to_string()]);
    assert!(committed_revisions(base.path()).is_empty());
}

/// A baseline in `dir` that recorded OBS holding `a` at md5 `1`.
fn recorded_one_file(dir: &Path) {
    fs::write(dir.join("obs-md5"), "a\t1\n").unwrap();
}

/// OBS's listing of a package at `rev` holding `a` at md5 `md5`.
fn obs_listing(rev: &str, md5: &str) -> String {
    format!(r#"<directory name="foo" srcmd5="{rev}"><entry name="a" md5="{md5}"/></directory>"#)
}

#[test]
fn a_restore_osc_reports_as_failed_but_obs_holds_is_recorded_as_landed() {
    let co = checkout_at("aaa");
    let base = tempfile::tempdir().unwrap();
    recorded_one_file(base.path());
    let res = record_restore(
        base.path(),
        co.path(),
        || {
            // osc wrote the new revision, then failed after the server had it.
            land(co.path(), "bbb");
            Err(Error::NotActionable("FileNotFoundError".into()))
        },
        || Ok(obs_listing("bbb", "1")),
    );
    assert!(res.is_ok());
    assert_eq!(restored_revisions(base.path()), vec!["bbb".to_string()]);
}

#[test]
fn a_failed_restore_is_still_a_failure_when_obs_does_not_hold_it() {
    let base = tempfile::tempdir().unwrap();
    recorded_one_file(base.path());
    // Nothing landed: OBS is still at the checkout's revision.
    let co = checkout_at("aaa");
    let res = record_restore(
        base.path(),
        co.path(),
        || Err(Error::NotActionable("refused".into())),
        || Ok(obs_listing("aaa", "1")),
    );
    assert!(res.is_err());
    // Another revision, but not the restored state.
    let co = checkout_at("aaa");
    let res = record_restore(
        base.path(),
        co.path(),
        || {
            land(co.path(), "bbb");
            Err(Error::NotActionable("failed".into()))
        },
        || Ok(obs_listing("bbb", "2")),
    );
    assert!(res.is_err());
    // Unreadable OBS: the failure stands.
    let res = record_restore(
        base.path(),
        co.path(),
        || Err(Error::NotActionable("failed".into())),
        || Err("timeout".to_string()),
    );
    assert!(res.is_err());
    assert!(restored_revisions(base.path()).is_empty());
}

#[test]
fn landed_despite_error_needs_a_newer_head_the_checkout_holds_and_the_restored_listing() {
    let rec = vec![("a".to_string(), "1".to_string())];
    let same = rec.clone();
    let other = vec![("a".to_string(), "2".to_string())];
    assert!(landed_despite_error("aaa", Some("bbb"), "bbb", &rec, &same));
    // Head is the revision before the commit: nothing landed.
    assert!(!landed_despite_error(
        "aaa",
        Some("aaa"),
        "aaa",
        &rec,
        &same
    ));
    // The checkout does not record the head.
    assert!(!landed_despite_error(
        "aaa",
        Some("ccc"),
        "bbb",
        &rec,
        &same
    ));
    assert!(!landed_despite_error("aaa", None, "bbb", &rec, &same));
    // Listing is not the restored state.
    assert!(!landed_despite_error(
        "aaa",
        Some("bbb"),
        "bbb",
        &rec,
        &other
    ));
    // Nothing recorded to compare with.
    assert!(!landed_despite_error("aaa", Some("bbb"), "bbb", &[], &[]));
}
