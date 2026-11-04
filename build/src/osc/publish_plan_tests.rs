use super::*;

/// Backups, build directories and stray files stay out of the commit.
/// The referenced patch goes in; the unreferenced one does not.
#[test]
fn only_the_package_files_are_published() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: pkg\nVersion: 1.0\nSource0: pkg-%{version}.tar.gz\nPatch0: 0001-fix.patch\nSource99: pkg-rpmlintrc\n";
    for (name, body) in [
        ("pkg.spec", spec),
        ("pkg-1.0.tar.gz", ""),
        ("_service", ""),
        ("pkg.changes", ""),
        ("pkg.rpmlintrc", ""),
        ("pkg-rpmlintrc", ""),
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
            "pkg-rpmlintrc",
            "pkg.changes",
            "pkg.spec",
        ]
    );
    assert_eq!(
        plan.skipped,
        [
            "build",
            "extra.patch",
            "pkg.changes.preexisting",
            "pkg.rpmlintrc",
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
    let err = commit(dir.path(), "msg", &[], None, Emit::quiet()).unwrap_err();
    assert!(matches!(err, Error::NothingToPublish(_)), "{err}");
}

#[test]
fn a_failed_ci_says_what_osc_printed() {
    // osc prints a malformed `_service` on stdout and exits 1, with stderr empty.
    let detail = ci_failure_detail("XML error in _service file on line 9, column 4\n", "");
    assert_eq!(detail, "XML error in _service file on line 9, column 4");
    assert_eq!(
        ci_failure_detail("", "  "),
        "osc printed nothing on stdout or stderr"
    );
    // Control characters never reach the error; stderr comes first.
    assert_eq!(ci_failure_detail("out", "err\x1b[0m"), "err\nout");
    let long: String = (0..30).map(|i| format!("line {i}\n")).collect();
    assert_eq!(ci_failure_detail(&long, "").lines().count(), 10);
}

#[test]
fn a_warning_on_stderr_does_not_hide_the_error_on_stdout() {
    // osc warns on stderr and reports the failure on stdout: both are shown.
    let detail = ci_failure_detail(
        "XML error in _service file on line 9, column 4\n",
        "warning: the spec is old\n",
    );
    assert_eq!(
        detail,
        "warning: the spec is old\nXML error in _service file on line 9, column 4"
    );
    // A line both streams print is shown once.
    assert_eq!(ci_failure_detail("same\nother\n", "same\n"), "same\nother");
}

/// elfutils has two specs in one package. The main one is at the new version
/// and its archive is named by its own `Name:`/`Version:`, not the sibling's
/// (still at the old version). The new archive is published.
#[test]
fn a_source_of_the_second_spec_is_named_by_that_spec() {
    let dir = tempfile::tempdir().unwrap();
    let main = "Name: elfutils\nVersion: 0.197\nSource: https://sourceware.org/elfutils/ftp/%{version}/%{name}-%{version}.tar.bz2\n";
    let sibling = "Name: elfutils-debuginfod\nVersion: 0.196\nSource: https://sourceware.org/elfutils/ftp/%{version}/elfutils-%{version}.tar.bz2\n";
    for (name, body) in [
        ("elfutils.spec", main),
        ("elfutils-debuginfod.spec", sibling),
        ("elfutils-0.197.tar.bz2", ""),
    ] {
        fs::write(dir.path().join(name), body).unwrap();
    }
    let plan = publish_plan(dir.path());
    assert!(
        plan.files.contains(&"elfutils-0.197.tar.bz2".to_string()),
        "{plan:?}"
    );
    assert!(!plan.skipped.contains(&"elfutils-0.197.tar.bz2".to_string()));
}

#[test]
fn each_spec_names_its_own_sources() {
    let main = "Name: elfutils\nVersion: 0.197\nSource: %{name}-%{version}.tar.bz2\n";
    let sibling =
        "Name: elfutils-debuginfod\nVersion: 0.196\nSource: elfutils-%{version}.tar.bz2\n";
    let names = all_spec_source_names(&[main.to_string(), sibling.to_string()]);
    assert_eq!(names, ["elfutils-0.197.tar.bz2", "elfutils-0.196.tar.bz2"]);
}

/// elfutils 0.197 was deleted from the checkout when the plan left it out: the
/// spec of that run named it only through the sibling's version. The expanded
/// spec names it, so the plan's skip is refused and the archive stays.
#[test]
fn an_archive_the_expanded_spec_names_is_kept_and_refused() {
    let dir = tempfile::tempdir().unwrap();
    // The checkout's own spec does not expand to the new archive.
    let stale = "Name: elfutils-debuginfod\nVersion: 0.196\nSource: elfutils-%{version}.tar.bz2\n";
    fs::write(dir.path().join("elfutils-debuginfod.spec"), stale).unwrap();
    fs::write(dir.path().join("elfutils-0.197.tar.bz2"), "").unwrap();
    let plan = publish_plan(dir.path());
    assert_eq!(plan.skipped, ["elfutils-0.197.tar.bz2"]);
    // rpmspec's expansion of the main spec names it.
    let expanded = "Name: elfutils\nVersion: 0.197\n\
        Source0: https://sourceware.org/elfutils/ftp/0.197/elfutils-0.197.tar.bz2\n";
    let named = spec_source_names(expanded);
    assert_eq!(
        named_but_skipped(&named, &plan.skipped),
        ["elfutils-0.197.tar.bz2"]
    );
    let gone = discard_untracked_archives(dir.path(), &plan.skipped, &named);
    assert!(gone.is_empty(), "{gone:?}");
    assert!(dir.path().join("elfutils-0.197.tar.bz2").exists());
}

/// An archive that nothing names is still deleted, as before.
#[test]
fn an_archive_nothing_names_is_still_discarded() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("old-1.0.tar.gz"), "").unwrap();
    let skipped = ["old-1.0.tar.gz".to_string()];
    let gone = discard_untracked_archives(dir.path(), &skipped, &[]);
    assert_eq!(gone, ["old-1.0.tar.gz"]);
    assert!(!dir.path().join("old-1.0.tar.gz").exists());
}
