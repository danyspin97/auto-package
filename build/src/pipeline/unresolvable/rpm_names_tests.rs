//! Which RPM files satisfy a dependency name a wait record waits on.

use super::rpm_names::*;

#[test]
fn an_rpm_file_name_splits_into_name_and_version() {
    assert_eq!(
        rpm_file_name_version("python313-pyinstaller-hooks-contrib-2026.7-32.1.noarch.rpm"),
        Some(("python313-pyinstaller-hooks-contrib", "2026.7"))
    );
    assert_eq!(
        rpm_file_name_version("python-foo-0.1-1.1.x86_64.rpm"),
        Some(("python-foo", "0.1"))
    );
    assert_eq!(rpm_file_name_version("foo-1.0-1.1.src.rpm"), None);
    assert_eq!(rpm_file_name_version("foo-1.0-1.1.x86_64.tar.gz"), None);
    assert_eq!(rpm_file_name_version("foo-bar.x86_64.rpm"), None);
}

#[test]
fn a_source_name_matches_its_rpm_under_any_flavour() {
    assert!(same_package("python313-foo", "foo"));
    assert!(same_package("python314-foo", "foo"));
    assert!(same_package("python-foo", "foo"));
    assert!(same_package("python3-foo", "python313-foo"));
    assert!(same_package(
        "python313-pyinstaller-hooks-contrib",
        "pyinstaller-hooks-contrib"
    ));
    assert!(same_package("python313-foo", "foo>=1.0"));
}

#[test]
fn a_name_does_not_match_a_longer_package_name() {
    assert!(!same_package("foo-bar", "foo"));
    assert!(!same_package("python313-foo-bar", "foo"));
    assert!(!same_package("python313-foo", "foo-bar"));
    assert!(!same_package("python313-foo", ""));
}

#[test]
fn a_flavoured_rpm_satisfies_a_bare_dependency_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("python313-foo-bar-1.0-1.1.noarch.rpm"), b"").unwrap();
    let dirs = [dir.path()];
    assert!(!wait_dep_satisfied(&dirs, "foo", &[]));
    std::fs::write(dir.path().join("python313-foo-2026.7-1.noarch.rpm"), b"").unwrap();
    assert!(wait_dep_satisfied(&dirs, "foo", &[]));
}

#[test]
fn a_version_bound_is_met_only_by_a_release_at_least_as_new() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("python313-foo-2026.6-1.noarch.rpm"), b"").unwrap();
    let dirs = [dir.path()];
    let bound = vec![("foo".to_string(), Some("2026.7".to_string()))];
    assert!(!wait_dep_satisfied(&dirs, "foo", &bound));
    std::fs::write(dir.path().join("python313-foo-2026.7-1.noarch.rpm"), b"").unwrap();
    assert!(wait_dep_satisfied(&dirs, "foo", &bound));
    // A bound with no version is not met by any RPM.
    let unknown = vec![("foo".to_string(), None)];
    assert!(!wait_dep_satisfied(&dirs, "foo", &unknown));
}

#[test]
fn the_local_build_directory_counts_as_well() {
    let top = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    std::fs::write(local.path().join("python314-foo-1.0-1.1.noarch.rpm"), b"").unwrap();
    assert!(wait_dep_satisfied(&[top.path(), local.path()], "foo", &[]));
    assert!(!wait_dep_satisfied(&[top.path()], "foo", &[]));
}
