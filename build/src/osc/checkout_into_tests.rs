use super::*;

/// `osc co` runs in `into_dir`, which the caller may not have made yet (a
/// work directory that is new, or a subdirectory such as `improve`).
#[test]
fn a_checkout_into_a_missing_directory_creates_it() {
    let base = tempfile::tempdir().unwrap();
    let into = base.path().join("work").join("improve");
    assert!(!into.exists());
    create_into_dir(&into).unwrap();
    assert!(into.is_dir());
}

#[test]
fn an_existing_directory_is_left_as_it_is() {
    let base = tempfile::tempdir().unwrap();
    fs::write(base.path().join("keep"), "x").unwrap();
    create_into_dir(base.path()).unwrap();
    assert!(base.path().join("keep").exists());
}

#[test]
fn a_directory_that_cannot_be_created_is_reported_with_its_path() {
    let base = tempfile::tempdir().unwrap();
    let blocker = base.path().join("file");
    fs::write(&blocker, "x").unwrap();
    let err = create_into_dir(&blocker.join("improve")).unwrap_err();
    assert!(
        err.to_string()
            .contains(&blocker.join("improve").display().to_string()),
        "{err}"
    );
}
