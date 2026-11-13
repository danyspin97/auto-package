use super::changelog_required::changes_needing_a_bullet;

const BEFORE: &str =
    "Name: foo\nVersion: 1.0\nBuildRequires: a\n\n%check\n%pytest\n\n%files\n%{_bindir}/foo\n";

fn needs(before: &str, after: &str) -> Vec<String> {
    changes_needing_a_bullet(before, after, false)
}

#[test]
fn dependency_and_comment_changes_need_no_bullet() {
    let after = BEFORE.replace(
        "BuildRequires: a\n",
        "# the build needs cmake 3.20\nBuildRequires: cmake >= 3.20\nBuildRequires(post): b\nRequires: c\n",
    );
    assert!(needs(BEFORE, &after).is_empty());
    let dropped = BEFORE.replace("BuildRequires: a\n", "");
    assert!(needs(BEFORE, &dropped).is_empty());
    // A conditional around a dependency only.
    let conditional = BEFORE.replace(
        "BuildRequires: a\n",
        "%if 0%{?suse_version} > 1600\nBuildRequires: a\n%endif\n",
    );
    assert!(needs(BEFORE, &conditional).is_empty());
}

#[test]
fn a_files_or_flag_change_needs_a_bullet() {
    let files = BEFORE.replace(
        "%{_bindir}/foo\n",
        "%{_bindir}/foo\n%{_mandir}/man1/foo.1*\n",
    );
    assert_eq!(needs(BEFORE, &files), ["%{_mandir}/man1/foo.1*"]);
    let mixed = BEFORE.replace("BuildRequires: a\n", "BuildRequires: a\nBuildRequires: z\n");
    assert!(needs(BEFORE, &mixed).is_empty());
}

#[test]
fn a_conditional_that_wraps_or_frees_other_lines_needs_a_bullet() {
    let wrapped = BEFORE.replace("%pytest\n", "%if 0\n%pytest\n%endif\n");
    assert_eq!(needs(BEFORE, &wrapped), ["%pytest", "%pytest"]);
    assert_eq!(needs(&wrapped, BEFORE), ["%pytest", "%pytest"]);
    // An unchanged wrapped line stays unchanged.
    assert!(needs(&wrapped, &wrapped).is_empty());
}

#[test]
fn a_line_moved_to_another_section_needs_a_bullet() {
    let moved = BEFORE.replace("%check\n%pytest\n\n", "%check\n\n%build\n%pytest\n\n");
    assert!(!needs(BEFORE, &moved).is_empty());
}

#[test]
fn a_patch_line_needs_no_bullet() {
    let patched = format!("{BEFORE}Patch0: x.patch\n");
    assert!(needs(BEFORE, &patched).is_empty());
}

#[test]
fn a_switch_covers_the_build_sections_only() {
    let before = "%build\n%configure\n%install\n%make_install\n%files\n/a\n";
    let after = "%build\n%meson\n%install\n%meson_install\n%files\n/a\n";
    assert!(!needs(before, after).is_empty());
    assert!(changes_needing_a_bullet(before, after, true).is_empty());
    let more = "%build\n%meson\n%install\n%meson_install\n%files\n/a\n/b\n";
    assert_eq!(changes_needing_a_bullet(before, more, true), ["/b"]);
}

#[test]
fn a_changed_patch_or_rpmlintrc_or_service_needs_a_bullet_a_new_patch_does_not_here() {
    use super::changelog_required::files_needing_a_bullet;
    use super::snapshot::CheckoutSnapshot;
    let dir = tempfile::tempdir().unwrap();
    for (n, c) in [
        ("a.patch", "old"),
        ("_service", "<services/>"),
        ("series", "a.patch\n"),
        ("foo.spec", "Name: foo\n"),
    ] {
        std::fs::write(dir.path().join(n), c).unwrap();
    }
    let snap = CheckoutSnapshot::take(dir.path());
    assert!(files_needing_a_bullet(dir.path(), Some(&snap), &[]).is_empty());
    // A spec change is the spec check's business; a new patch is named elsewhere.
    std::fs::write(dir.path().join("foo.spec"), "Name: bar\n").unwrap();
    std::fs::write(dir.path().join("b.patch"), "new").unwrap();
    assert!(files_needing_a_bullet(dir.path(), Some(&snap), &[]).is_empty());
    std::fs::write(dir.path().join("a.patch"), "changed").unwrap();
    std::fs::write(dir.path().join("series"), "a.patch\nb.patch\n").unwrap();
    assert_eq!(
        files_needing_a_bullet(dir.path(), Some(&snap), &[]),
        ["a.patch"],
        "series changes with a patch file"
    );
    std::fs::write(dir.path().join("a.patch"), "old").unwrap();
    assert_eq!(
        files_needing_a_bullet(dir.path(), Some(&snap), &[]),
        ["series"]
    );
    std::fs::write(dir.path().join("_service"), "<services>x</services>").unwrap();
    std::fs::write(
        dir.path().join("foo-rpmlintrc"),
        "# reason: y\naddFilter(\"y\")\n",
    )
    .unwrap();
    assert_eq!(
        files_needing_a_bullet(dir.path(), Some(&snap), &[]),
        ["_service", "foo-rpmlintrc", "series"]
    );
    // A file the turn staged is not listed.
    assert_eq!(
        files_needing_a_bullet(dir.path(), Some(&snap), &["_service".to_string()]),
        ["foo-rpmlintrc", "series"]
    );
}

/// A print in %check that a fix round adds needs no `.changes` bullet.
#[test]
fn a_build_print_added_to_check_needs_no_bullet() {
    let before = "Name: x\n%check\nmake check\n";
    let after = "Name: x\n%check\nmake check\ncat test-suite.log\n";
    assert!(changes_needing_a_bullet(before, after, false).is_empty());
    let flag = "Name: x\n%check\nmake check CFLAGS=-O0\n";
    // The replaced line and its new form both need a bullet.
    assert_eq!(
        changes_needing_a_bullet(before, flag, false),
        ["make check", "make check CFLAGS=-O0"]
    );
}

/// A spec with an archive name and a `%setup -n` directory, and a `%check`.
const SOURCES: &str = "Name: foo\nVersion: 1.0\nSource0: https://files.pythonhosted.org/packages/source/F/Foo/Foo-%{version}.tar.gz\n%prep\n%autosetup -n Foo-%{version}\n%check\n%pytest\n%files\n/a\n";

#[test]
fn an_archive_name_that_tracks_the_release_needs_no_bullet() {
    // The case, the `-` for `_`, and the PyPI normalised name of the archive.
    let lower = SOURCES
        .replace(
            "packages/source/F/Foo/Foo-%{version}.tar.gz",
            "packages/source/f/foo/foo-%{version}.tar.gz",
        )
        .replace("-n Foo-%{version}", "-n foo-%{version}");
    assert!(needs(SOURCES, &lower).is_empty());
    let zip = SOURCES.replace("Foo-%{version}.tar.gz", "Foo-%{version}.zip");
    assert!(needs(SOURCES, &zip).is_empty());
    let underscore = SOURCES.replace("Foo-%{version}.tar.gz", "foo_bar-%{version}.tar.gz");
    let dashed = SOURCES.replace("Foo-%{version}.tar.gz", "foo-bar-%{version}.tar.gz");
    assert!(needs(&underscore, &dashed).is_empty());
    let setup = SOURCES.replace("-n Foo-%{version}", "-n foo-%{version}");
    assert!(needs(SOURCES, &setup).is_empty());
}

#[test]
fn an_archive_host_or_project_change_needs_a_bullet() {
    let host = SOURCES.replace(
        "https://files.pythonhosted.org/packages/source/F/Foo/Foo-%{version}.tar.gz",
        "https://github.com/foo/foo/archive/Foo-%{version}.tar.gz",
    );
    assert!(!needs(SOURCES, &host).is_empty());
    let project = SOURCES.replace("Foo-%{version}.tar.gz", "Bar-%{version}.tar.gz");
    assert!(!needs(SOURCES, &project).is_empty());
    let setup = SOURCES.replace("-n Foo-%{version}", "-n Bar-%{version}");
    assert!(!needs(SOURCES, &setup).is_empty());
    // A new Source that is not an archive is a file, not a rename.
    let file = SOURCES.replace("Name: foo\n", "Name: foo\nSource1: foo.desktop\n");
    assert!(!needs(SOURCES, &file).is_empty());
}

#[test]
fn a_workaround_removed_from_check_needs_no_bullet() {
    let sed = SOURCES.replace("%pytest\n", "sed -i '/import mock/d' tests/x.py\n%pytest\n");
    assert!(needs(&sed, SOURCES).is_empty());
    let export = SOURCES.replace("%pytest\n", "export FOO_OFF=1\n%pytest\n");
    assert!(needs(&export, SOURCES).is_empty());
    let mock = SOURCES.replace("%pytest\n", "%{__python3} -c \"import mock\"\n%pytest\n");
    assert!(needs(&mock, SOURCES).is_empty());
    let masked = SOURCES.replace("%pytest\n", "make check || true\n");
    let unmasked = SOURCES.replace("%pytest\n", "make check\n");
    assert!(needs(&masked, &unmasked).is_empty());
}

#[test]
fn a_workaround_that_skips_a_test_or_is_still_used_needs_a_bullet() {
    let sed = SOURCES.replace("%pytest\n", "sed -i '/x/d' tests/x.py\n%pytest\n");
    // Removing the sed is the workaround; adding a deselect to the test is not.
    let deselect = SOURCES.replace("%pytest\n", "%pytest --deselect tests/x.py::t\n");
    assert!(!needs(&sed, &deselect).is_empty());
    let skip = SOURCES.replace("%pytest\n", "%pytest -k \"not slow\"\n");
    assert!(!needs(SOURCES, &skip).is_empty());
    // A removed test run is a removed test.
    let no_test = SOURCES.replace("%pytest\n", "");
    assert!(!needs(SOURCES, &no_test).is_empty());
    // The export is used elsewhere in the spec.
    let used = SOURCES.replace("%pytest\n", "export PYVER=3\n%pytest $PYVER\n");
    assert!(!needs(&used, SOURCES).is_empty());
    // PYTHONPATH is read by the tools without a name in the spec.
    let implicit = SOURCES.replace("%pytest\n", "export PYTHONPATH=%{buildroot}\n%pytest\n");
    assert!(!needs(&implicit, SOURCES).is_empty());
    // A mask replaced by another command, or a chain, is not a removal.
    let chained = SOURCES.replace("%pytest\n", "make check || true\n");
    let other = SOURCES.replace("%pytest\n", "make test\n");
    assert!(!needs(&chained, &other).is_empty());
    let chain = SOURCES.replace("%pytest\n", "sed -i 's/a/b/' x && %pytest\n");
    assert!(!needs(&chain, SOURCES).is_empty());
    // A sed outside the build sections is not housekeeping.
    let install = SOURCES.replace("%files\n", "%install\nsed -i 's/a/b/' x\n%files\n");
    assert!(!needs(&install, SOURCES).is_empty());
}
