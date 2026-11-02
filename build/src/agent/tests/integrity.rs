//! Tests for the copy of a checkout's osc and git metadata that a turn is
//! checked against: an unchanged copy passes, a change is undone and the turn
//! rejected, and the pipeline's own writes between turns are not a change.

use std::os::unix::fs::PermissionsExt;

/// A checkout with a `.osc` (the `_apiurl` and a `_files` list) and a `.git`
/// with a config.
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let osc = dir.path().join(".osc");
    std::fs::create_dir(&osc).unwrap();
    std::fs::write(osc.join("_apiurl"), "https://api.example\n").unwrap();
    std::fs::write(osc.join("_files"), "<directory/>\n").unwrap();
    std::fs::set_permissions(osc.join("_apiurl"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let git = dir.path().join(".git");
    std::fs::create_dir_all(git.join("objects").join("ab")).unwrap();
    std::fs::write(git.join("config"), "[core]\n").unwrap();
    std::fs::write(git.join("objects").join("ab").join("cdef"), b"blob").unwrap();
    dir
}

/// Run `during` as a turn on `dir` and return what the check says.
fn turn_with(dir: &std::path::Path, during: impl FnOnce(&std::path::Path)) -> Option<String> {
    let guard = crate::agent::integrity::MetadataGuard::begin(Some(dir), false);
    during(dir);
    guard.settle()
}

#[test]
fn an_unchanged_checkout_passes() {
    let dir = checkout();
    assert_eq!(turn_with(dir.path(), |_| {}), None);
    // A pipeline or agent read changes nothing either.
    assert_eq!(
        turn_with(dir.path(), |d| {
            let _ = std::fs::read(d.join(".osc/_apiurl"));
        }),
        None
    );
}

#[test]
fn no_checkout_means_nothing_is_checked() {
    let guard = crate::agent::integrity::MetadataGuard::begin(None, false);
    assert_eq!(guard.settle(), None);
}

#[test]
fn a_rewritten_apiurl_is_put_back_byte_for_byte_and_the_turn_rejected() {
    let dir = checkout();
    let path = dir.path().join(".osc/_apiurl");
    let detail = turn_with(dir.path(), |d| {
        std::fs::write(d.join(".osc/_apiurl"), "http://evil\n").unwrap();
    })
    .expect("a rewritten _apiurl is a change");
    assert!(detail.contains("modified .osc/_apiurl"), "{detail}");
    assert_eq!(std::fs::read(&path).unwrap(), b"https://api.example\n");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_file_added_under_osc_is_removed() {
    let dir = checkout();
    let detail = turn_with(dir.path(), |d| {
        std::fs::write(d.join(".osc/newfile"), "x").unwrap();
    })
    .expect("an added file is a change");
    assert!(detail.contains("added .osc/newfile"), "{detail}");
    assert!(!dir.path().join(".osc/newfile").exists());
}

#[test]
fn a_deleted_file_is_put_back() {
    let dir = checkout();
    let detail = turn_with(dir.path(), |d| {
        std::fs::remove_file(d.join(".osc/_files")).unwrap();
    })
    .expect("a deleted file is a change");
    assert!(detail.contains("removed .osc/_files"), "{detail}");
    assert_eq!(
        std::fs::read(dir.path().join(".osc/_files")).unwrap(),
        b"<directory/>\n"
    );
}

#[test]
fn a_mode_change_is_undone() {
    let dir = checkout();
    let path = dir.path().join(".osc/_files");
    let detail = turn_with(dir.path(), |_| {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    })
    .expect("a mode change is a change");
    assert!(detail.contains("modified .osc/_files"), "{detail}");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn a_link_created_into_osc_is_removed_and_a_link_over_a_file_is_undone() {
    let dir = checkout();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, "s").unwrap();
    let detail = turn_with(dir.path(), |d| {
        std::os::unix::fs::symlink(&secret, d.join(".osc/link")).unwrap();
        std::fs::remove_file(d.join(".osc/_apiurl")).unwrap();
        std::os::unix::fs::symlink(&secret, d.join(".osc/_apiurl")).unwrap();
    })
    .expect("links are changes");
    assert!(detail.contains("added .osc/link"), "{detail}");
    assert!(
        !dir.path().join(".osc/link").exists()
            && std::fs::symlink_metadata(dir.path().join(".osc/link")).is_err()
    );
    let apiurl = dir.path().join(".osc/_apiurl");
    assert!(
        !std::fs::symlink_metadata(&apiurl)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&apiurl).unwrap(), b"https://api.example\n");
    assert_eq!(std::fs::read(&secret).unwrap(), b"s");
}

#[test]
fn a_directory_replaced_by_a_link_is_restored_with_its_files() {
    let dir = checkout();
    let outside = tempfile::tempdir().unwrap();
    let detail = turn_with(dir.path(), |d| {
        std::fs::create_dir(d.join(".git/hooks")).unwrap();
        std::fs::write(d.join(".git/hooks/post-update"), "#!/bin/sh\n").unwrap();
        std::fs::remove_dir_all(d.join(".git/objects")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.join(".git/objects")).unwrap();
    })
    .expect("the replaced store is a change");
    assert!(detail.contains("added .git/hooks"), "{detail}");
    assert!(!dir.path().join(".git/hooks").exists());
    assert_eq!(
        std::fs::read(dir.path().join(".git/objects/ab/cdef")).unwrap(),
        b"blob"
    );
    assert!(
        std::fs::symlink_metadata(dir.path().join(".git/objects"))
            .unwrap()
            .is_dir()
    );
}

#[test]
fn a_git_object_is_put_back() {
    let dir = checkout();
    let detail = turn_with(dir.path(), |d| {
        std::fs::write(d.join(".git/objects/ab/cdef"), "a longer blob").unwrap();
    })
    .expect("an object change is a change");
    assert!(detail.contains("modified .git/objects/ab/cdef"), "{detail}");
    assert_eq!(
        std::fs::read(dir.path().join(".git/objects/ab/cdef")).unwrap(),
        b"blob"
    );
}

#[test]
fn a_large_git_pack_change_is_reported_but_cannot_be_put_back() {
    let dir = checkout();
    let pack = dir.path().join(".git/objects/pack");
    std::fs::create_dir(&pack).unwrap();
    let big = vec![0u8; 16 * 1024 * 1024 + 1];
    std::fs::write(pack.join("p.pack"), &big).unwrap();
    let detail = turn_with(dir.path(), |d| {
        std::fs::write(d.join(".git/objects/pack/p.pack"), b"short").unwrap();
    })
    .expect("a pack change is a change");
    assert!(
        detail.contains("modified .git/objects/pack/p.pack"),
        "{detail}"
    );
    assert!(
        detail.contains("could not restore .git/objects/pack/p.pack"),
        "{detail}"
    );
}

#[test]
fn the_pipelines_own_writes_before_a_turn_are_not_a_change() {
    let dir = checkout();
    // An osc call between turns rewrites the metadata; the next turn's copy
    // is taken after it, so the change is not the agent's.
    std::fs::write(
        dir.path().join(".osc/_files"),
        "<directory>new</directory>\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".osc/_osclib_cache"), "fresh").unwrap();
    assert_eq!(turn_with(dir.path(), |_| {}), None);
    assert_eq!(
        std::fs::read(dir.path().join(".osc/_files")).unwrap(),
        b"<directory>new</directory>\n"
    );
}

#[test]
fn a_large_osc_source_change_is_reported_as_not_put_back() {
    // Over the kept size and not a git object: compared by length and digest,
    // so the change is reported, and the detail says the bytes are gone.
    let dir = checkout();
    let sources = dir.path().join(".osc/sources");
    std::fs::create_dir(&sources).unwrap();
    std::fs::write(sources.join("big.tar"), vec![0u8; 16 * 1024 * 1024 + 1]).unwrap();
    let detail = turn_with(dir.path(), |d| {
        std::fs::write(d.join(".osc/sources/big.tar"), b"x").unwrap();
    })
    .expect("a change to a large source is a change");
    assert!(
        detail.contains("could not restore .osc/sources/big.tar"),
        "{detail}"
    );
}

#[test]
fn a_service_a_turn_may_not_edit_is_put_back_and_the_turn_rejected() {
    let dir = checkout();
    let service = dir.path().join("_service");
    std::fs::write(&service, "<services/>\n").unwrap();
    let guard = crate::agent::integrity::MetadataGuard::begin(Some(dir.path()), true);
    std::fs::write(&service, "<services><service name=\"x\"/></services>\n").unwrap();
    let detail = guard.settle().expect("a changed _service is a change");
    assert!(detail.contains("modified _service"), "{detail}");
    assert_eq!(std::fs::read(&service).unwrap(), b"<services/>\n");
}

#[test]
fn a_service_a_turn_may_edit_is_not_checked_by_the_guard() {
    let dir = checkout();
    std::fs::write(dir.path().join("_service"), "<services/>\n").unwrap();
    assert_eq!(
        turn_with(dir.path(), |d| {
            std::fs::write(d.join("_service"), "<services/>\n<x/>\n").unwrap();
        }),
        None
    );
}
