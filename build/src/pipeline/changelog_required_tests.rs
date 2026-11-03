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
