
use super::*;

#[test]
fn extracts_the_filename_from_oscs_package_file_conflict_message() {
    let stderr = "osc: failed to add file 'python-foo.changes' \
                       file/dir with the same name already exists\n";
    assert_eq!(changes_conflict_file(stderr), Some("python-foo.changes"));
}

#[test]
fn returns_none_for_unrelated_errors() {
    assert_eq!(changes_conflict_file("404 Not Found"), None);
}

#[test]
fn a_moved_aside_changes_file_lands_outside_the_checkout_and_is_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("proj").join("python-foo");
    fs::create_dir_all(&checkout).unwrap();
    fs::write(checkout.join("python-foo.changes"), "local").unwrap();

    let first = move_aside(&checkout, "python-foo.changes").unwrap();
    assert!(!first.starts_with(&checkout));
    assert!(!checkout.join("python-foo.changes").exists());
    assert_eq!(fs::read_to_string(&first).unwrap(), "local");

    fs::write(checkout.join("python-foo.changes"), "again").unwrap();
    let second = move_aside(&checkout, "python-foo.changes").unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(&first).unwrap(), "local");
    assert_eq!(fs::read_to_string(&second).unwrap(), "again");
}

#[test]
fn only_a_changes_file_is_ever_moved_aside() {
    let spec = "osc: failed to add file 'python-foo.spec' \
                    file/dir with the same name already exists\n";
    assert_eq!(changes_conflict_file(spec), None);
    let path = "osc: failed to add file 'sub/python-foo.changes' \
                    file/dir with the same name already exists\n";
    assert_eq!(changes_conflict_file(path), None);
}
