use super::*;

/// The `_history` of duckdb's branch on OBS: the newest entry is last.
const HISTORY: &str = r#"<revisionlist>
  <revision rev="1" vrev="1"><srcmd5>9fdc</srcmd5><version>unknown</version><time>1793635403</time><user>dspinella</user></revision>
  <revision rev="2" vrev="2"><srcmd5>7617</srcmd5><version>unknown</version><time>1793635918</time><user>dspinella</user>
    <comment>auto-package: Update to 2.0.0 * Too many changes to list, see</comment></revision>
  <revision rev="7" vrev="7"><srcmd5>64fe</srcmd5><version>unknown</version><time>1793642305</time><user>dspinella</user>
    <comment>auto-package: Update to 2.0.0 * Too many changes to list,
      see</comment></revision>
</revisionlist>"#;

#[test]
fn the_newest_revision_of_a_history_is_the_last_entry() {
    let latest = latest_revision(HISTORY).unwrap();
    assert_eq!(latest.rev, "7");
    assert_eq!(latest.user, "dspinella");
    assert_eq!(latest.time, 1793642305);
    assert_eq!(
        latest.comment,
        "auto-package: Update to 2.0.0 * Too many changes to list, see"
    );
    assert_eq!(latest_revision("<revisionlist/>"), None);
}

#[test]
fn utc_stamp_is_the_time_osc_log_shows() {
    // Revision 2 of the same branch: `osc log` shows it at 16:11:58 UTC.
    assert_eq!(utc_stamp(1793635918), "2026-11-02 16:11:58");
    assert_eq!(utc_stamp(0), "1970-01-01 00:00:00");
}

/// A baseline of `foo.spec` with one failed attempt noted in it.
fn baseline_with_attempt() -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    note_attempt(base.path(), &["foo.spec".to_string()]).unwrap();
    base
}

#[test]
fn a_foreign_head_is_named_with_its_author_time_and_the_baseline() {
    let base = baseline_with_attempt();
    let why = moved_on_reason(
        "home:p",
        "foo",
        "aaa",
        "bbb",
        None,
        base.path(),
        latest_revision(HISTORY).ok_or_else(String::new),
    );
    assert!(
        why.starts_with("OBS moved on: home:p/foo is at revision aaa, the checkout at bbb"),
        "{why}"
    );
    assert!(
        why.contains("newest OBS revision 7 by dspinella at 2026-11-02 "),
        "{why}"
    );
    assert!(
        why.contains("the baseline kept 1 attempted file(s) (foo.spec)"),
        "{why}"
    );
    assert!(why.ends_with("nothing was written to OBS"), "{why}");
}

#[test]
fn a_head_an_attempt_committed_is_said_so() {
    let co = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(co.path().join(".osc")).unwrap();
    let rev = |r: &str| format!(r#"<directory name="foo" srcmd5="{r}"></directory>"#);
    std::fs::write(co.path().join(".osc/_files"), rev("aaa")).unwrap();
    let base = baseline_with_attempt();
    record_commit(base.path(), co.path(), || {
        std::fs::write(co.path().join(".osc/_files"), rev("ccc")).unwrap();
        Ok(())
    })
    .unwrap();
    // The checkout does not hold the attempt's revision.
    std::fs::write(co.path().join(".osc/_files"), rev("aaa")).unwrap();
    let why = moved_on_reason(
        "home:p",
        "foo",
        "ccc",
        "aaa",
        None,
        base.path(),
        Err("no history".into()),
    );
    assert!(
        why.contains("ccc is a revision an attempt of this package committed"),
        "{why}"
    );
    assert!(why.contains("but the checkout does not hold it"), "{why}");
    assert!(!why.contains("newest OBS revision"), "{why}");
}

#[test]
fn a_commit_in_flight_is_named_as_a_possible_own_commit() {
    let co = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(co.path().join(".osc")).unwrap();
    std::fs::write(
        co.path().join(".osc/_files"),
        r#"<directory name="foo" srcmd5="aaa"></directory>"#,
    )
    .unwrap();
    let base = baseline_with_attempt();
    // The commit was cut short: its marker is left, nothing was recorded.
    let _ = record_commit(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    let why = moved_on_reason(
        "home:p",
        "foo",
        "ddd",
        "aaa",
        None,
        base.path(),
        Err("x".into()),
    );
    assert!(why.contains("may not have been recorded"), "{why}");
}

fn revision_with(user: &str, comment: &str) -> LatestRevision {
    LatestRevision {
        rev: "8".to_string(),
        srcmd5: "9e1f".to_string(),
        user: user.to_string(),
        time: 0,
        comment: comment.to_string(),
    }
}

#[test]
fn a_head_is_a_restore_of_this_user_by_its_message_and_user() {
    let restore = revision_with("dspinella", RESTORE_MESSAGE);
    assert!(restore_head_by(&restore, "dspinella"));
    assert!(!restore_head_by(&restore, "someone-else"));
    let other = revision_with("dspinella", "auto-package: Update to 2.0.0");
    assert!(!restore_head_by(&other, "dspinella"));
}
