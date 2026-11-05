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
    record_restore(base.path(), co.path(), || {
        land(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    assert_eq!(restored_revisions(base.path()), vec!["bbb".to_string()]);
    assert!(committed_revisions(base.path()).is_empty());
}
