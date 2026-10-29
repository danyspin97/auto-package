
use super::*;

#[test]
fn the_newest_changes_entry_is_matched_by_its_message() {
    let sep = "-".repeat(67);
    let changes = format!(
        "{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 2.0\n\n{sep}\n\
             Mon Jan  1 00:00:00 UTC 2023 - A <a@b.c>\n\n- Update to 1.0\n"
    );
    assert!(newest_changes_entry_says(&changes, "Update to 2.0"));
    // An older entry does not count, nor does a different version.
    assert!(!newest_changes_entry_says(&changes, "Update to 1.0"));
    assert!(!newest_changes_entry_says(&changes, "Update to 2.0.1"));
    assert!(!newest_changes_entry_says("", "Update to 2.0"));
}

#[test]
fn the_package_name_is_what_precedes_version_and_release() {
    assert_eq!(
        rpm_base_name("python313-foo-bar-1.0.2-2.1.noarch.rpm"),
        Some("python313-foo-bar")
    );
    assert_eq!(rpm_base_name("libfoo1-3-1.1.x86_64.rpm"), Some("libfoo1"));
    assert_eq!(rpm_base_name("notes.txt"), None);
    assert_eq!(rpm_base_name("x.rpm"), None);
}

#[test]
fn every_file_of_a_listed_package_goes_and_everything_else_stays() {
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "python313-foo-1.0-1.1.noarch.rpm",
        "python313-foo-2.0-1.1.noarch.rpm",
        "python313-foobar-1.0-1.1.noarch.rpm",
        "other-3-1.1.x86_64.rpm",
        "python-foo-1.0-1.1.src.rpm",
    ] {
        fs::write(dir.path().join(f), "").unwrap();
    }
    remove_superseded_rpms(
        dir.path(),
        &["python313-foo-2.0-1.1.noarch.rpm".to_string()],
    );
    let mut left: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "other-3-1.1.x86_64.rpm",
            "python-foo-1.0-1.1.src.rpm",
            "python313-foobar-1.0-1.1.noarch.rpm",
        ]
    );
}
