use crate::osc::changes::*;
use std::fs;

#[test]
fn the_package_changes_file_is_the_one_named_after_the_checkout() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("dbus-1");
    fs::create_dir_all(&dir).unwrap();
    // The subpackage's file sorts first; the package's own is the one to write.
    fs::write(dir.join("dbus-1-devel-doc.changes"), "").unwrap();
    fs::write(dir.join("dbus-1.changes"), "").unwrap();
    assert_eq!(newest_changes_file(&dir), Some(dir.join("dbus-1.changes")));
}

#[test]
fn without_the_package_file_the_first_changes_file_is_used() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("pkg");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("b.changes"), "").unwrap();
    fs::write(dir.join("a.changes"), "").unwrap();
    assert_eq!(newest_changes_file(&dir), Some(dir.join("a.changes")));
}

#[test]
fn a_package_without_a_changes_file_has_none() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("elfutils");
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(newest_changes_file(&dir), None);
}
