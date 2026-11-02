use super::*;

/// A checkout as osc leaves it: `.osc/_files` lists the tracked names and
/// `.osc/sources` holds their pristine copies.
fn checkout(tracked: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let entries: String = tracked
        .iter()
        .map(|(name, _)| format!(r#"<entry name="{name}" md5="x"/>"#))
        .collect();
    fs::create_dir_all(dir.path().join(".osc/sources")).unwrap();
    fs::write(
        dir.path().join(".osc/_files"),
        format!("<directory>{entries}</directory>"),
    )
    .unwrap();
    for (name, body) in tracked {
        fs::write(dir.path().join(".osc/sources").join(name), body).unwrap();
        fs::write(dir.path().join(name), body).unwrap();
    }
    dir
}

#[test]
fn the_baseline_keeps_each_tracked_file_as_osc_has_it() {
    let co = checkout(&[("foo.spec", "Name: foo\n")]);
    // The run edits the working copy before its first write.
    fs::write(co.path().join("foo.spec"), "edited").unwrap();
    let base = tempfile::tempdir().unwrap();
    save_baseline(co.path(), base.path()).unwrap();
    assert_eq!(
        fs::read_to_string(base.path().join("files/foo.spec")).unwrap(),
        "Name: foo\n"
    );
    assert_eq!(
        fs::read_to_string(base.path().join("tracked")).unwrap(),
        "foo.spec"
    );
}

#[test]
fn a_run_that_wrote_nothing_restores_nothing() {
    let co = checkout(&[("foo.spec", "a")]);
    let base = tempfile::tempdir().unwrap();
    save_baseline(co.path(), base.path()).unwrap();
    let res = restore_baseline_with(
        base.path(),
        co.path(),
        |_| Ok(()),
        |_, _, _| unreachable!("nothing was attempted, so nothing is committed"),
    );
    assert_eq!(res, Ok(()));
}

#[test]
fn a_failed_attempt_is_undone_and_its_new_file_removed() {
    let co = checkout(&[("foo.spec", "Name: foo\n"), ("foo-1.0.tar.gz", "old")]);
    let base = tempfile::tempdir().unwrap();
    save_baseline(co.path(), base.path()).unwrap();
    // The attempt: spec edited, old archive dropped, new archive added.
    fs::write(co.path().join("foo.spec"), "Name: foo\nVersion: 2\n").unwrap();
    fs::remove_file(co.path().join("foo-1.0.tar.gz")).unwrap();
    fs::write(co.path().join("foo-2.0.tar.gz"), "new").unwrap();
    let attempt: Vec<String> = ["foo.spec", "foo-1.0.tar.gz", "foo-2.0.tar.gz"]
        .map(String::from)
        .to_vec();
    note_attempt(base.path(), &attempt).unwrap();
    // The attempt landed on OBS: osc now lists the new archive, not the old.
    write_files_xml(co.path(), &["foo.spec", "foo-2.0.tar.gz"]);

    let mut sent = Vec::new();
    let res = restore_baseline_with(
        base.path(),
        co.path(),
        |_| Ok(()),
        |_, message, files| {
            sent.push((message.to_string(), files.to_vec()));
            Ok(())
        },
    );
    assert_eq!(res, Ok(()));
    assert_eq!(
        fs::read_to_string(co.path().join("foo.spec")).unwrap(),
        "Name: foo\n"
    );
    assert_eq!(
        fs::read_to_string(co.path().join("foo-1.0.tar.gz")).unwrap(),
        "old"
    );
    assert!(!co.path().join("foo-2.0.tar.gz").exists());
    assert_eq!(sent, vec![(RESTORE_MESSAGE.to_string(), attempt)]);
}

#[test]
fn an_attempted_file_that_never_reached_obs_is_left_alone() {
    let co = checkout(&[("foo.spec", "Name: foo\n")]);
    let base = tempfile::tempdir().unwrap();
    save_baseline(co.path(), base.path()).unwrap();
    // The commit failed before it landed: OBS does not list the new file.
    fs::write(co.path().join("foo-2.0.tar.gz"), "new").unwrap();
    note_attempt(base.path(), &["foo-2.0.tar.gz".to_string()]).unwrap();
    let res = restore_baseline_with(
        base.path(),
        co.path(),
        |_| Ok(()),
        |_, _, _| unreachable!("nothing reached OBS, so nothing is committed"),
    );
    assert_eq!(res, Ok(()));
    assert!(co.path().join("foo-2.0.tar.gz").exists());
}

#[test]
fn begin_baseline_replaces_an_earlier_one_for_the_same_package() {
    let co = checkout(&[("foo.spec", "a")]);
    let root = tempfile::tempdir().unwrap();
    let first =
        begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap();
    let second =
        begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap();
    assert_eq!(first, second);
    assert!(attempted_files(&second).is_empty());
    assert!(second.join("files/foo.spec").exists());
}

#[test]
fn discard_pending_removes_only_a_pending_baseline() {
    let co = checkout(&[("foo.spec", "a")]);
    let root = tempfile::tempdir().unwrap();
    let kept = begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap();
    assert!(!discard_pending(&kept).unwrap());
    assert!(kept.exists());
    note_attempt(&kept, &["foo.spec".to_string()]).unwrap();
    assert!(discard_pending(&kept).unwrap());
    assert!(!kept.exists());
}

#[test]
fn begin_baseline_refuses_a_pending_one_without_a_reason() {
    // The process exited during a restore: the attempt is noted, but no
    // reason was written. The next baseline must not replace it.
    let co = checkout(&[("foo.spec", "a")]);
    let root = tempfile::tempdir().unwrap();
    let kept = begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap();
    note_attempt(&kept, &["../escape".to_string()]).unwrap();
    assert!(is_pending(&kept));
    assert!(!kept.join(UNRESTORED).exists());
    let err =
        begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap_err();
    assert!(err.contains(&kept.display().to_string()), "{err}");
    assert!(kept.join("files/foo.spec").exists());
}

#[test]
fn begin_baseline_keeps_an_unrestored_one_and_refuses() {
    let co = checkout(&[("foo.spec", "a")]);
    let root = tempfile::tempdir().unwrap();
    let kept = begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap();
    // An attempt whose restore cannot run (a file name that is not plain):
    // no OBS call is made, the restore fails and marks the baseline.
    note_attempt(&kept, &["../escape".to_string()]).unwrap();
    assert!(restore_baseline(&kept, co.path(), Emit::quiet()).is_err());
    let err =
        begin_baseline(root.path(), co.path(), "home:u:Base", "foo", Emit::quiet()).unwrap_err();
    assert!(err.contains(&kept.display().to_string()), "{err}");
    assert_eq!(attempted_files(&kept), vec!["../escape".to_string()]);
    assert!(kept.join("files/foo.spec").exists());
}

/// Rewrite `.osc/_files` as osc would list `names` after a commit.
fn write_files_xml(checkout: &Path, names: &[&str]) {
    let entries: String = names
        .iter()
        .map(|n| format!(r#"<entry name="{n}" md5="x"/>"#))
        .collect();
    fs::write(
        checkout.join(".osc/_files"),
        format!("<directory>{entries}</directory>"),
    )
    .unwrap();
}

#[test]
fn a_tracked_file_without_a_saved_copy_is_not_guessed_back() {
    let co = checkout(&[("foo.spec", "a")]);
    fs::remove_file(co.path().join(".osc/sources/foo.spec")).unwrap();
    let base = tempfile::tempdir().unwrap();
    save_baseline(co.path(), base.path()).unwrap();
    note_attempt(base.path(), &["foo.spec".to_string()]).unwrap();
    let err = restore_baseline_with(
        base.path(),
        co.path(),
        |_| Ok(()),
        |_, _, _| unreachable!("the baseline cannot be restored, so nothing is committed"),
    )
    .unwrap_err();
    assert!(err.contains("no saved copy"), "{err}");
}

#[test]
fn a_baseline_saved_from_another_checkout_is_refused_and_nothing_is_written() {
    // The restore writes into the path the baseline names, so a baseline that
    // names another checkout than the caller's is refused before any write.
    let co = checkout(&[("foo.spec", "a")]);
    let other = checkout(&[("foo.spec", "b")]);
    let base = tempfile::tempdir().unwrap();
    save_baseline(other.path(), base.path()).unwrap();
    note_attempt(base.path(), &["foo.spec".to_string()]).unwrap();
    let err = restore_baseline_with(
        base.path(),
        co.path(),
        |_| Ok(()),
        |_, _, _| unreachable!("a refused restore commits nothing"),
    )
    .unwrap_err();
    assert!(err.contains("not from"), "{err}");
    assert_eq!(fs::read_to_string(co.path().join("foo.spec")).unwrap(), "a");
}
