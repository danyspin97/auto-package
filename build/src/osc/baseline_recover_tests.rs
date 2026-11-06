//! The restore of a baseline against OBS, through its seams: OBS's listing is
//! given as XML, and the commit records what it would send.

use super::*;
use std::cell::RefCell;

/// A checkout at OBS revision `rev` with `foo.spec` (pristine copy kept).
fn checkout_at(rev: &str) -> tempfile::TempDir {
    let co = tempfile::tempdir().unwrap();
    fs::create_dir_all(co.path().join(".osc/sources")).unwrap();
    fs::write(co.path().join(".osc/_project"), "home:p\n").unwrap();
    fs::write(co.path().join(".osc/_package"), "foo\n").unwrap();
    write_rev(co.path(), rev);
    fs::write(co.path().join(".osc/sources/foo.spec"), "Name: foo\n").unwrap();
    fs::write(co.path().join("foo.spec"), "Name: foo\n").unwrap();
    co
}

/// What osc writes to `.osc/_files` once a commit lands at `rev`.
fn write_rev(checkout: &Path, rev: &str) {
    fs::write(
        checkout.join(".osc/_files"),
        format!(r#"<directory name="foo" rev="{rev}" srcmd5="{rev}"><entry name="foo.spec" md5="x"/></directory>"#),
    )
    .unwrap();
}

/// OBS's listing of a link: its own revision `lsrcmd5`, the expanded `xsrcmd5`,
/// and `foo.spec` with md5 `md5` (`x` is what the baseline recorded, `y` what
/// an attempt sent).
fn link_listing(expanded: &str, md5: &str) -> String {
    format!(
        r#"<directory name="foo" rev="7" srcmd5="lsrc"><linkinfo project="p" package="foo" lsrcmd5="lsrc" xsrcmd5="{expanded}"/><entry name="foo.spec" md5="{md5}"/></directory>"#
    )
}

/// A baseline of `co` with one attempt that changed `foo.spec`.
fn attempted(co: &Path) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    save_baseline(co, base.path()).unwrap();
    fs::write(co.join("foo.spec"), "Name: foo\nVersion: 2\n").unwrap();
    note_attempt(base.path(), &["foo.spec".to_string()]).unwrap();
    base
}

fn history(rev: &str, user: &str) -> LatestRevision {
    LatestRevision {
        rev: rev.into(),
        srcmd5: String::new(),
        user: user.into(),
        time: 1793642305,
        comment: "auto-package: Update".into(),
    }
}

/// Runs the restore with OBS at `listing` and records the commits it makes.
fn restore(
    base: &Path,
    co: &Path,
    listing: &str,
    commits: &RefCell<Vec<String>>,
) -> Result<Restored, String> {
    restore_baseline_listing(
        base,
        co,
        |_| Ok(listing.to_string()),
        |c| head_matches_checkout(c, base, listing, |_, _| Ok(history("9", "someone"))),
        |_, message, files| {
            commits
                .borrow_mut()
                .push(format!("{message}: {}", files.join(",")));
            Ok(())
        },
    )
}

/// The attempt's commit landed: osc recorded the new revision in the checkout.
#[test]
fn an_attempts_own_commit_is_undone_on_its_own() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    record_commit(base.path(), co.path(), || {
        write_rev(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    let commits = RefCell::new(Vec::new());
    let out = restore(base.path(), co.path(), &link_listing("bbb", "y"), &commits);
    assert_eq!(out, Ok(Restored::Undone));
    assert_eq!(commits.borrow().len(), 1);
    assert!(commits.borrow()[0].starts_with(RESTORE_MESSAGE));
    assert_eq!(
        fs::read_to_string(co.path().join("foo.spec")).unwrap(),
        "Name: foo\n"
    );
}

/// The attempt's commit landed and osc recorded it (a link, so its unexpanded
/// directory srcmd5 differs from the revision osc records): the restore goes on.
#[test]
fn a_link_whose_attempt_landed_is_not_taken_for_a_moved_on_obs() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    record_commit(base.path(), co.path(), || {
        write_rev(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    let commits = RefCell::new(Vec::new());
    let out = restore(base.path(), co.path(), &link_listing("bbb", "y"), &commits);
    assert_eq!(out, Ok(Restored::Undone));
}

/// Someone committed after the attempt: the restore is refused, nothing is
/// written, and the reason names the foreign revision.
#[test]
fn a_foreign_commit_after_the_attempt_is_refused_with_its_revision() {
    let co = checkout_at("bbb");
    let base = attempted(co.path());
    let commits = RefCell::new(Vec::new());
    let out = restore(base.path(), co.path(), &link_listing("ccc", "y"), &commits);
    let why = out.unwrap_err();
    assert!(
        why.starts_with("OBS moved on: home:p/foo is at revision ccc, the checkout at bbb"),
        "{why}"
    );
    assert!(why.contains("newest OBS revision 9 by someone"), "{why}");
    assert!(
        why.contains("the baseline kept 1 attempted file(s) (foo.spec)"),
        "{why}"
    );
    assert!(commits.borrow().is_empty());
    assert!(base.path().join("attempted").exists());
}

/// A baseline recorded before the commits were recorded: OBS and the checkout
/// both hold `bbb`, but nothing says an attempt committed it. It is not taken
/// for the attempt's own, so the restore is refused and nothing is written.
#[test]
fn an_old_record_without_revisions_is_refused_even_when_the_checkout_holds_obs() {
    let co = checkout_at("bbb");
    let base = attempted(co.path());
    assert!(!base.path().join("revisions").exists());
    let commits = RefCell::new(Vec::new());
    let why = restore(base.path(), co.path(), &link_listing("bbb", "y"), &commits).unwrap_err();
    assert!(
        why.contains("no attempt of this package committed"),
        "{why}"
    );
    assert!(commits.borrow().is_empty());
}

/// The checkout was refreshed from OBS (`osc up`) after the attempt, and OBS
/// holds a commit by someone else: equal revisions are not ownership.
#[test]
fn a_checkout_refreshed_to_a_foreign_commit_is_refused() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    record_commit(base.path(), co.path(), || {
        write_rev(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    write_rev(co.path(), "hhh");
    let commits = RefCell::new(Vec::new());
    let why = restore(base.path(), co.path(), &link_listing("hhh", "y"), &commits).unwrap_err();
    assert!(why.contains("hhh"), "{why}");
    assert!(
        why.contains("no attempt of this package committed"),
        "{why}"
    );
    assert!(commits.borrow().is_empty());
}

/// Two commits in one run: the first landed (recorded as `bbb`), the second was
/// cut short with its marker at `bbb`. OBS still holds the first attempt, so
/// that attempt is put back: the baseline is not dropped as never landed.
#[test]
fn a_cut_short_second_commit_does_not_drop_the_first_attempts_undo() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    record_commit(base.path(), co.path(), || {
        write_rev(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    let _ = record_commit(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    let commits = RefCell::new(Vec::new());
    let out = restore(base.path(), co.path(), &link_listing("bbb", "y"), &commits);
    assert_eq!(out, Ok(Restored::Undone));
    assert_eq!(commits.borrow().len(), 1);
}

/// The refusal names the command that discards the baseline, quoted for the
/// shell, whatever kept the baseline.
#[test]
fn a_refused_restore_names_the_discard_command() {
    let cmd = discard_command("foo", "home:p", None, std::path::Path::new("/w s"));
    assert_eq!(
        cmd,
        "auto-package baseline discard foo --project home:p --work-dir '/w s'"
    );
    assert_eq!(
        with_discard_hint("OBS moved on", &cmd),
        format!("OBS moved on; if OBS is right as it is, discard the baseline with `{cmd}`")
    );
}

/// A crash between the marker and the commit: the commit never landed, so OBS
/// is still at the revision the marker holds. Nothing is committed.
#[test]
fn a_crash_between_the_marker_and_the_commit_writes_nothing() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    let _ = record_commit(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    let commits = RefCell::new(Vec::new());
    assert_eq!(
        restore(base.path(), co.path(), &link_listing("aaa", "x"), &commits),
        Ok(Restored::NothingWritten)
    );
    assert!(commits.borrow().is_empty());
}

/// The same crash, and then someone else committed: refused, not dropped.
#[test]
fn a_crash_then_a_foreign_commit_is_refused() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    let _ = record_commit(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    let commits = RefCell::new(Vec::new());
    let out = restore(base.path(), co.path(), &link_listing("ddd", "y"), &commits);
    assert!(out.unwrap_err().contains("OBS moved on"));
    assert!(commits.borrow().is_empty());
}

/// The marker alone settles it: OBS is at the revision the marker holds, even
/// when its listing no longer matches the recorded one.
#[test]
fn a_marker_at_obs_revision_settles_a_commit_that_never_landed() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    let _ = record_commit(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    let commits = RefCell::new(Vec::new());
    assert_eq!(
        restore(base.path(), co.path(), &link_listing("aaa", "z"), &commits),
        Ok(Restored::NothingWritten)
    );
    assert!(commits.borrow().is_empty());
}

/// The restore's commit landed (osc recorded its revision) and the run ended
/// before the baseline was removed. The next run must not refuse it: OBS holds
/// the restored revision, so there is nothing left to put back.
#[test]
fn a_restore_that_landed_before_a_crash_is_not_refused_again() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    record_commit(base.path(), co.path(), || {
        write_rev(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    record_restore(base.path(), co.path(), || {
        write_rev(co.path(), "ccc");
        Ok(())
    })
    .unwrap();
    assert_eq!(restored_revisions(base.path()), vec!["ccc".to_string()]);
    let commits = RefCell::new(Vec::new());
    assert_eq!(
        restore(base.path(), co.path(), &link_listing("ccc", "z"), &commits),
        Ok(Restored::NothingWritten)
    );
    assert!(commits.borrow().is_empty());
}

/// A restore cut short before its commit: OBS still holds the attempt's
/// recorded revision, so the restore is made again, and nothing else is.
#[test]
fn a_restore_cut_short_before_its_commit_is_made_again() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    record_commit(base.path(), co.path(), || {
        write_rev(co.path(), "bbb");
        Ok(())
    })
    .unwrap();
    let _ = record_restore(base.path(), co.path(), || {
        Err(Error::NotActionable("killed".into()))
    });
    assert!(restored_revisions(base.path()).is_empty());
    let commits = RefCell::new(Vec::new());
    assert_eq!(
        restore(base.path(), co.path(), &link_listing("bbb", "y"), &commits),
        Ok(Restored::Undone)
    );
    assert_eq!(commits.borrow().len(), 1);
}

/// A commit that lands during the restore: the head is not the revision the
/// restore made, so the baseline stays for inspection.
#[test]
fn a_commit_landing_during_the_restore_keeps_the_baseline() {
    let dir = tempfile::tempdir().unwrap();
    assert!(landed_as_made(dir.path(), "home:p/foo", "ccc", "ccc").is_ok());
    let why = landed_as_made(dir.path(), "home:p/foo", "ccc", "hhh").unwrap_err();
    assert!(
        why.contains("a commit landed on OBS during the restore"),
        "{why}"
    );
    assert!(why.contains("the baseline is kept in"), "{why}");
}

/// OBS holds the empty package, which no attempt committed: the restore goes
/// ahead over it, and puts the baseline's tracked file back.
#[test]
fn an_empty_obs_revision_is_restored_over_not_refused() {
    let co = checkout_at("aaa");
    let base = attempted(co.path());
    let empty = format!(r#"<directory name="foo" rev="3" srcmd5="{EMPTY_SRCMD5}"></directory>"#);
    fs::write(co.path().join(".osc/_files"), &empty).unwrap();
    let commits = RefCell::new(Vec::new());
    let out = restore(base.path(), co.path(), &empty, &commits);
    assert_eq!(out, Ok(Restored::Undone));
    assert_eq!(commits.borrow().len(), 1);
    assert!(commits.borrow()[0].starts_with(RESTORE_MESSAGE));
    assert_eq!(
        fs::read_to_string(co.path().join("foo.spec")).unwrap(),
        "Name: foo\n"
    );
}

/// The decision alone: the empty revision goes ahead when the checkout holds
/// it, and an unknown non-empty revision never does.
#[test]
fn only_the_empty_or_an_attempts_revision_may_be_restored_over() {
    let attempts = vec!["bbb".to_string()];
    assert!(may_restore_over(EMPTY_SRCMD5, EMPTY_SRCMD5, &[]));
    assert!(may_restore_over("bbb", "bbb", &attempts));
    // An unknown non-empty revision, held by the checkout too, is refused.
    assert!(!may_restore_over("ccc", "ccc", &attempts));
    assert!(!may_restore_over("ccc", "ccc", &[]));
    // The empty revision the checkout does not hold is refused.
    assert!(!may_restore_over(EMPTY_SRCMD5, "aaa", &[]));
    assert!(!may_restore_over("bbb", "ccc", &attempts));
}
