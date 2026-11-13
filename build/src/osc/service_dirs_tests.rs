use super::*;

const CARGO_VENDOR_SRC: &str = r#"<services>
  <service name="cargo_vendor" mode="manual">
    <param name="src">pleamar</param>
    <param name="compression">zst</param>
  </service>
</services>"#;

fn checkout_with(service: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("_service"), service).unwrap();
    fs::write(dir.path().join("pleamar.spec"), "Name: pleamar\n").unwrap();
    dir
}

#[test]
fn an_untracked_dir_named_by_service_src_is_removed() {
    let dir = checkout_with(CARGO_VENDOR_SRC);
    fs::create_dir_all(dir.path().join("pleamar/src")).unwrap();
    fs::write(dir.path().join("pleamar/Cargo.toml"), "").unwrap();
    let removed = remove_service_source_dirs(dir.path(), &[]);
    assert_eq!(removed, ["pleamar"]);
    assert!(!dir.path().join("pleamar").exists());
    assert!(dir.path().join("pleamar.spec").exists());
}

#[test]
fn an_untracked_dir_not_named_by_the_service_is_kept_and_still_skipped() {
    let dir = checkout_with(CARGO_VENDOR_SRC);
    fs::create_dir(dir.path().join("other")).unwrap();
    fs::write(dir.path().join("other/file.txt"), "").unwrap();
    let removed = remove_service_source_dirs(dir.path(), &[]);
    assert!(removed.is_empty());
    assert!(dir.path().join("other").is_dir());
    // The publish plan leaves it out, so the commit still warns about it.
    assert!(
        publish_plan(dir.path())
            .skipped
            .contains(&"other".to_string())
    );
}

#[test]
fn a_param_other_than_a_source_dir_names_nothing() {
    let dir = checkout_with(
        r#"<services><service name="tar_scm" mode="manual"><param name="subdir">pleamar</param></service></services>"#,
    );
    fs::create_dir(dir.path().join("pleamar")).unwrap();
    assert!(remove_service_source_dirs(dir.path(), &[]).is_empty());
    assert!(dir.path().join("pleamar").is_dir());
}

#[test]
fn a_tracked_dir_is_never_removed() {
    let dir = checkout_with(CARGO_VENDOR_SRC);
    fs::create_dir(dir.path().join(".osc")).unwrap();
    fs::write(
        dir.path().join(".osc/_files"),
        r#"<directory name="pleamar"><entry name="pleamar" md5="b" size="1"/></directory>"#,
    )
    .unwrap();
    fs::create_dir(dir.path().join("pleamar")).unwrap();
    assert!(remove_service_source_dirs(dir.path(), &[]).is_empty());
    assert!(dir.path().join("pleamar").is_dir());
}

#[test]
fn a_dot_dir_is_never_removed() {
    let dir = checkout_with(
        r#"<services><service name="cargo_vendor" mode="manual"><param name="src">.osc</param></service></services>"#,
    );
    fs::create_dir(dir.path().join(".osc")).unwrap();
    assert!(remove_service_source_dirs(dir.path(), &[]).is_empty());
    assert!(dir.path().join(".osc").is_dir());
}

#[test]
fn a_dir_the_spec_names_is_kept() {
    let dir = checkout_with(CARGO_VENDOR_SRC);
    fs::create_dir(dir.path().join("pleamar")).unwrap();
    let named = vec!["pleamar".to_string()];
    assert!(remove_service_source_dirs(dir.path(), &named).is_empty());
    assert!(dir.path().join("pleamar").is_dir());
}

#[test]
fn a_symlink_named_by_the_service_is_not_followed() {
    let dir = checkout_with(CARGO_VENDOR_SRC);
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep.txt"), "").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("pleamar")).unwrap();
    assert!(remove_service_source_dirs(dir.path(), &[]).is_empty());
    assert!(outside.path().join("keep.txt").exists());
    assert!(dir.path().join("pleamar").is_symlink());
}

#[test]
fn a_param_with_an_entity_is_not_read_as_a_name() {
    let names = service_source_dirs(
        r#"<services><service name="cargo_vendor"><param name="src">a&amp;b</param></service></services>"#,
    );
    assert!(names.is_empty());
    let names = service_source_dirs(CARGO_VENDOR_SRC);
    assert_eq!(names, ["pleamar"]);
}
