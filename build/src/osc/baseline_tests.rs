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

/// An OBS `/source` listing with the given files and md5s.
fn obs_listing(files: &[(&str, &str)]) -> String {
    let entries: String = files
        .iter()
        .map(|(name, md5)| format!(r#"<entry name="{name}" md5="{md5}"/>"#))
        .collect();
    format!("<directory>{entries}</directory>")
}

/// A checkout with `foo.spec` (md5 `x`), its baseline, and a failed attempt
/// that changed the spec and was noted before the commit failed.
fn failed_attempt() -> (tempfile::TempDir, tempfile::TempDir) {
    let co = checkout(&[("foo.spec", "Name: foo\n")]);
    let base = tempfile::tempdir().unwrap();
    save_baseline(co.path(), base.path()).unwrap();
    fs::write(co.path().join("foo.spec"), "Name: foo\nVersion: 2\n").unwrap();
    note_attempt(base.path(), &["foo.spec".to_string()]).unwrap();
    (co, base)
}

/// The listing OBS holds is the one the baseline recorded and the commit
/// failed: nothing was written, so the attempt is dropped without a commit.
#[test]
fn a_rerun_continues_when_obs_still_holds_the_baseline() {
    let (co, base) = failed_attempt();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Ok(obs_listing(&[("foo.spec", "x")])),
        |_| unreachable!("OBS matches the baseline: the revision check is not needed"),
        |_, _, _| unreachable!("nothing landed: nothing is committed"),
    );
    assert_eq!(res, Ok(Restored::NothingWritten));
    // The failed attempt's edit is taken out of the checkout, and the saved
    // copies stay until the caller has settled the baseline.
    assert_eq!(
        fs::read_to_string(co.path().join("foo.spec")).unwrap(),
        "Name: foo\n"
    );
    assert!(base.path().join("files/foo.spec").exists());
}

/// A tracked file the attempt removed locally comes back too, when OBS matches.
#[test]
fn a_rerun_that_continues_also_restores_a_removed_tracked_file() {
    let (co, base) = failed_attempt();
    fs::remove_file(co.path().join("foo.spec")).unwrap();
    note_attempt(base.path(), &["foo.spec".to_string()]).unwrap();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Ok(obs_listing(&[("foo.spec", "x")])),
        |_| unreachable!("OBS matches the baseline"),
        |_, _, _| unreachable!("nothing landed: nothing is committed"),
    );
    assert_eq!(res, Ok(Restored::NothingWritten));
    assert_eq!(
        fs::read_to_string(co.path().join("foo.spec")).unwrap(),
        "Name: foo\n"
    );
}

/// Someone else changed the spec on OBS: the record does not match, and the
/// revision check refuses as before.
#[test]
fn a_file_changed_on_obs_is_still_refused() {
    let (co, base) = failed_attempt();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Ok(obs_listing(&[("foo.spec", "other")])),
        |_| Err("OBS moved on".to_string()),
        |_, _, _| unreachable!("refused before any write"),
    );
    assert_eq!(res, Err("OBS moved on".to_string()));
}

/// A new file on OBS that the baseline does not list is someone else's write.
#[test]
fn a_file_added_on_obs_is_still_refused() {
    let (co, base) = failed_attempt();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Ok(obs_listing(&[("foo.spec", "x"), ("bar.tar.gz", "y")])),
        |_| Err("OBS moved on".to_string()),
        |_, _, _| unreachable!("refused before any write"),
    );
    assert_eq!(res, Err("OBS moved on".to_string()));
}

/// A baseline kept before the listing was recorded says nothing about OBS now.
#[test]
fn a_baseline_without_a_recorded_listing_is_refused() {
    let (co, base) = failed_attempt();
    fs::remove_file(base.path().join("obs-md5")).unwrap();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Ok(obs_listing(&[("foo.spec", "x")])),
        |_| Err("OBS moved on".to_string()),
        |_, _, _| unreachable!("refused before any write"),
    );
    assert_eq!(res, Err("OBS moved on".to_string()));
}

/// The record matches OBS, but the baseline was saved from another checkout.
#[test]
fn a_listing_match_for_another_checkout_is_refused() {
    let (_co, base) = failed_attempt();
    let other = tempfile::tempdir().unwrap();
    let res = restore_baseline_listing(
        base.path(),
        other.path(),
        |_| Ok(obs_listing(&[("foo.spec", "x")])),
        |_| Err("not this checkout".to_string()),
        |_, _, _| unreachable!("refused before any write"),
    );
    let why = res.expect_err("another checkout must be refused");
    assert!(why.contains("saved from"), "{why}");
}

/// OBS unreadable: the revision check decides, as before.
#[test]
fn an_unreadable_obs_listing_falls_back_to_the_revision_check() {
    let (co, base) = failed_attempt();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Err("network".to_string()),
        |_| Err("OBS moved on".to_string()),
        |_, _, _| unreachable!("refused before any write"),
    );
    assert_eq!(res, Err("OBS moved on".to_string()));
}

/// The attempt did land and OBS shows it: the saved copy goes back and is
/// committed, as before.
#[test]
fn an_attempt_that_landed_is_still_undone() {
    let (co, base) = failed_attempt();
    let mut committed = Vec::new();
    let res = restore_baseline_listing(
        base.path(),
        co.path(),
        |_| Ok(obs_listing(&[("foo.spec", "landed")])),
        |_| Ok(()),
        |_, _, files| {
            committed = files.to_vec();
            Ok(())
        },
    );
    assert_eq!(res, Ok(Restored::Undone));
    assert_eq!(committed, ["foo.spec"]);
    assert_eq!(
        fs::read_to_string(co.path().join("foo.spec")).unwrap(),
        "Name: foo\n"
    );
}

#[test]
fn listing_md5s_reads_names_and_md5s_sorted() {
    let xml = obs_listing(&[("b.spec", "2"), ("a.patch", "1")]);
    assert_eq!(
        listing_md5s(&xml),
        [
            ("a.patch".to_string(), "1".to_string()),
            ("b.spec".to_string(), "2".to_string())
        ]
    );
}

/// A link's unexpanded listing (`/source/<p>/<pkg>`): its directory srcmd5 is
/// the link's own revision, its `xsrcmd5` the expanded one osc records.
const LINK_LISTING: &str = r#"<directory name="duckdb" rev="7" srcmd5="64fe82a2">
  <linkinfo project="server:database" package="duckdb" srcmd5="f584" baserev="f584" xsrcmd5="706cd735" lsrcmd5="64fe82a2"/>
  <entry name="duckdb.spec" md5="a"/>
</directory>"#;

#[test]
fn a_links_expanded_revision_is_its_xsrcmd5() {
    assert_eq!(expanded_revision(LINK_LISTING).as_deref(), Some("706cd735"));
}

#[test]
fn a_plain_packages_revision_is_its_directory_srcmd5() {
    let xml = r#"<directory name="foo" rev="3" srcmd5="abc123"><entry name="foo.spec" md5="a"/></directory>"#;
    assert_eq!(expanded_revision(xml).as_deref(), Some("abc123"));
    assert_eq!(expanded_revision("<directory/>"), None);
}
