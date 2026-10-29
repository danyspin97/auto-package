
use super::*;

/// Backups, build directories and stray files stay out of the commit.
/// The referenced patch goes in; the unreferenced one does not.
#[test]
fn only_the_package_files_are_published() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: pkg\nVersion: 1.0\nSource0: pkg-%{version}.tar.gz\nPatch0: 0001-fix.patch\n";
    for (name, body) in [
        ("pkg.spec", spec),
        ("pkg-1.0.tar.gz", ""),
        ("_service", ""),
        ("pkg.changes", ""),
        ("pkg.rpmlintrc", ""),
        ("0001-fix.patch", ""),
        ("extra.patch", ""),
        ("pkg.spec.preexisting", spec),
        ("pkg.changes.preexisting", ""),
        ("stray.txt", ""),
    ] {
        fs::write(dir.path().join(name), body).unwrap();
    }
    fs::create_dir(dir.path().join(".osc")).unwrap();
    fs::create_dir(dir.path().join("build")).unwrap();

    let plan = publish_plan(dir.path());

    assert_eq!(
        plan.files,
        [
            "0001-fix.patch",
            "_service",
            "pkg-1.0.tar.gz",
            "pkg.changes",
            "pkg.rpmlintrc",
            "pkg.spec",
        ]
    );
    assert_eq!(
        plan.skipped,
        [
            "build",
            "extra.patch",
            "pkg.changes.preexisting",
            "pkg.spec.preexisting",
            "stray.txt",
        ]
    );
}

/// A `Source:` the service fetched (the upstream `LICENSE`) is published,
/// by its URL's last segment, its `#/` rename, or a macro pattern.
#[test]
fn a_file_a_source_line_names_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: pkg\n\
            Source0: https://example.org/pkg-%{version}.tar.gz\n\
            Source2: https://raw.githubusercontent.com/o/r/%{version}/LICENSE\n\
            Source3: https://example.org/dl?id=7#/pkg-data.json\n\
            Source4: data-%{version}.bin\n\
            # Source5: commented.txt\n";
    for (name, body) in [
        ("pkg.spec", spec),
        ("LICENSE", ""),
        ("pkg-data.json", ""),
        ("data-1.2.bin", ""),
        ("commented.txt", ""),
        ("LICENSE.bak", ""),
    ] {
        fs::write(dir.path().join(name), body).unwrap();
    }

    let plan = publish_plan(dir.path());

    assert_eq!(
        plan.files,
        ["LICENSE", "data-1.2.bin", "pkg-data.json", "pkg.spec"]
    );
    assert_eq!(plan.skipped, ["LICENSE.bak", "commented.txt"]);
}

/// `Version` is known, so another release's archive is not "named by the
/// spec" just because the Source line says `%{version}`.
#[test]
fn the_spec_version_is_not_a_wildcard() {
    let names = spec_source_names(
        "Name: python-pkg\nVersion: 2.3.0\nSource: https://x/pkg-%{version}.tar.gz\n",
    );
    assert_eq!(names, ["pkg-2.3.0.tar.gz"]);
    assert!(
        !names
            .iter()
            .any(|p| matches_source_name(p, "pkg-1.27.2.tar.gz"))
    );
}

#[test]
fn a_macro_in_a_source_name_matches_any_text() {
    assert!(matches_source_name("pkg-%{version}.dat", "pkg-1.0.dat"));
    assert!(matches_source_name("%{name}-%version.dat", "x-1.dat"));
    assert!(matches_source_name("LICENSE", "LICENSE"));
    assert!(!matches_source_name(
        "pkg-%{version}.dat",
        "pkg-1.0.dat.bak"
    ));
    assert!(!matches_source_name("pkg-%{version}.dat", "other-1.0.dat"));
    assert!(!matches_source_name("LICENSE", "LICENSE.bak"));
    assert!(!matches_source_name("a%{x}a", "a"));
}

#[test]
fn a_patch_named_by_the_series_file_is_published() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pkg.spec"), "Name: pkg\n").unwrap();
    fs::write(dir.path().join("series"), "# quilt\n0002-quilt.patch\n").unwrap();
    fs::write(dir.path().join("0002-quilt.patch"), "").unwrap();

    let plan = publish_plan(dir.path());

    assert!(
        plan.files.contains(&"0002-quilt.patch".to_string()),
        "{plan:?}"
    );
}

#[test]
fn nothing_is_committed_without_a_file_to_commit() {
    let dir = tempfile::tempdir().unwrap();
    let err = commit(dir.path(), "msg", &[], Emit::quiet()).unwrap_err();
    assert!(matches!(err, Error::NothingToPublish(_)), "{err}");
}
