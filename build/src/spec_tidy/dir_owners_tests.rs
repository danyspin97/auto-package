use super::dir_owners::{MissingDir, missing_dirs, own_new_dirs};
use super::standard_dirs::{STANDARD_DIRS, is_standard_dir};

/// A spec with `files` after the `%install` of the jay package.
fn spec(files: &str) -> String {
    format!(
        "Name:           jay\nVersion:        1.2.3\nRelease:        0\n\
         Summary:        A compositor\nLicense:        GPL-3.0-only\n\n\
         %description\nA compositor.\n\n%prep\n%autosetup\n\n%build\n\n\
         %install\ninstall -Dm0755 jay %{{buildroot}}%{{_bindir}}/jay\n\
         install -Dm0644 jay.desktop %{{buildroot}}%{{_datadir}}/wayland-sessions/jay.desktop\n\
         \n%files\n{files}\n%changelog\n"
    )
}

fn texts(spec: &str) -> Vec<String> {
    missing_dirs(spec).into_iter().map(|m| m.text).collect()
}

const JAY_FILES: &str = "%{_bindir}/jay\n\
    %{_datadir}/wayland-sessions/jay.desktop\n\
    %{_datadir}/xdg-desktop-portal/portals/jay.portal\n\
    %{_datadir}/xdg-desktop-portal/jay-portals.conf\n";

#[test]
fn the_jay_spec_misses_its_three_directories_parents_first() {
    let found = missing_dirs(&spec(JAY_FILES));
    let texts: Vec<_> = found.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "%{_datadir}/wayland-sessions",
            "%{_datadir}/xdg-desktop-portal",
            "%{_datadir}/xdg-desktop-portal/portals",
        ]
    );
    assert_eq!(found[2].path, "/usr/share/xdg-desktop-portal/portals");
}

#[test]
fn a_dir_line_owns_its_directory() {
    let files = format!("%dir %{{_datadir}}/wayland-sessions\n{JAY_FILES}");
    assert_eq!(
        texts(&spec(&files)),
        [
            "%{_datadir}/xdg-desktop-portal",
            "%{_datadir}/xdg-desktop-portal/portals"
        ]
    );
}

#[test]
fn a_dir_line_for_the_main_package_is_not_repeated() {
    let files = "%dir %{_datadir}/x\n%{_datadir}/x/a.conf\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn a_whole_directory_entry_owns_the_tree() {
    let files = "%{_datadir}/x/\n%{_datadir}/x/y/z.conf\n";
    assert!(texts(&spec(files)).is_empty());
    let files = "%{_datadir}/x\n%{_datadir}/x/y/z.conf\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn a_glob_owns_what_it_matches_but_not_its_own_parent() {
    let files = "%{_datadir}/x/*\n%{_datadir}/x/y/z.conf\n";
    assert_eq!(texts(&spec(files)), ["%{_datadir}/x"]);
    let files = "%dir %{_datadir}/x\n%{_datadir}/x/*\n%{_datadir}/x/y/z.conf\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn a_ghost_path_needs_an_owner_for_its_parent() {
    let files = "%ghost %{_sysconfdir}/x/state\n";
    assert_eq!(texts(&spec(files)), ["%{_sysconfdir}/x"]);
}

#[test]
fn ownership_in_another_subpackage_counts() {
    let text = spec("%{_bindir}/jay\n").replace(
        "%changelog\n",
        "%files -n jay-data\n%dir %{_datadir}/x\n\n%changelog\n",
    );
    let text = text.replace("%{_bindir}/jay\n", "%{_bindir}/jay\n%{_datadir}/x/a.conf\n");
    assert!(texts(&text).is_empty());
}

#[test]
fn a_files_list_file_skips_the_whole_spec() {
    let text = spec(JAY_FILES).replace("%files\n", "%files -f jay.lang\n");
    assert!(missing_dirs(&text).is_empty());
}

#[test]
fn an_unknown_macro_skips_the_entry() {
    let files = "%{python_sitelib}/x/a.py\n%{_datadir}/%{other}/b\n%{_datadir}/y/c\n";
    assert_eq!(texts(&spec(files)), ["%{_datadir}/y"]);
    assert_eq!(texts(&spec("$RPM_BUILD_ROOT/x/y\n")), Vec::<String>::new());
}

#[test]
fn a_standard_parent_is_left_alone() {
    let files = "%{_unitdir}/x.service\n%{_userunitdir}/x.service\n%{_mandir}/man1/x.1\n\
        %{_datadir}/applications/x.desktop\n%{_datadir}/icons/hicolor/48x48/apps/x.png\n\
        %{_prefix}/lib/sysusers.d/x.conf\n%{_libdir}/pkgconfig/x.pc\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn a_dir_line_inside_an_if_block_owns_for_every_branch() {
    let files = "%if 0%{?x}\n%dir %{_datadir}/x\n%endif\n%{_datadir}/x/a.conf\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn directives_before_the_path_are_stripped() {
    let files = "%attr(0755,root,root) %{_bindir}/x\n\
        %attr(0644, root, root) %config(noreplace) %{_sysconfdir}/x/x.conf\n\
        %verify(not md5) %lang(de) %{_datadir}/x/de.mo\n";
    assert_eq!(texts(&spec(files)), ["%{_sysconfdir}/x", "%{_datadir}/x"]);
}

#[test]
fn relative_doc_and_license_paths_are_skipped() {
    let files = "%doc README.md docs/guide.md\n%license LICENSE\n%doc %{_docdir}/x/a\n";
    assert_eq!(texts(&spec(files)), ["%{_docdir}/x"]);
}

#[test]
fn exclude_and_defattr_lines_are_ignored() {
    let files = "%defattr(-,root,root)\n%exclude %{_datadir}/x/a\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn name_and_version_expand_from_the_preamble() {
    let files = "%{_datadir}/%{name}/a\n%{_docdir}/%{name}-%{version}/b\n";
    let found = missing_dirs(&spec(files));
    let paths: Vec<_> = found.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        paths,
        ["/usr/share/jay", "/usr/share/doc/packages/jay-1.2.3"]
    );
}

#[test]
fn a_spec_without_files_has_nothing_missing() {
    assert!(missing_dirs("Name: x\nVersion: 1\n\n%description\nx\n").is_empty());
    assert!(missing_dirs("").is_empty());
}

#[test]
fn crlf_lines_are_read() {
    let text = spec(JAY_FILES).replace('\n', "\r\n");
    assert_eq!(texts(&text).len(), 3);
}

#[test]
fn the_line_is_the_first_entry_under_the_parent() {
    let text = spec(JAY_FILES);
    let found = missing_dirs(&text);
    let lines: Vec<&str> = text.split('\n').collect();
    let at = |m: &MissingDir| lines[m.line].to_string();
    assert!(at(&found[0]).ends_with("wayland-sessions/jay.desktop"));
    assert!(at(&found[1]).ends_with("portals/jay.portal"));
    assert_eq!(found[1].line, found[2].line);
}

#[test]
fn fhs_roots_and_legacy_dirs_are_standard() {
    let files = "/bin/x\n/sbin/x\n/etc/xinetd.d/x\n/usr/games/x\n/var/db/x\n";
    assert!(texts(&spec(files)).is_empty());
}

#[test]
fn a_section_with_doc_or_license_owns_its_doc_and_license_dirs() {
    let files = "%doc %{_docdir}/%{name}/README\n%license %{_datadir}/licenses/%{name}/COPYING\n";
    assert!(texts(&spec(files)).is_empty());
    let files = "%{_docdir}/%{name}/README\n%{_datadir}/licenses/%{name}/COPYING\n";
    assert_eq!(
        texts(&spec(files)),
        ["%{_docdir}/%{name}", "%{_datadir}/licenses/%{name}"]
    );
}

#[test]
fn a_subpackage_doc_section_owns_the_subpackage_doc_dir() {
    let text = spec("%doc %{_docdir}/%{name}/README\n").replace(
        "%changelog\n",
        "%files devel\n%doc %{_docdir}/%{name}-devel/README\n\n%changelog\n",
    );
    assert!(texts(&text).is_empty());
}

#[test]
fn a_backslash_continued_line_is_read_as_one_line() {
    let text = spec("%{_bindir}/jay \\\n%{_datadir}/x/a.conf\n");
    let found = missing_dirs(&text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "/usr/share/x");
    let first = text
        .split('\n')
        .position(|l| l.starts_with("%{_bindir}/jay"))
        .unwrap();
    assert_eq!(found[0].line, first);
}

#[test]
fn a_trailing_backslash_is_not_kept_in_the_word() {
    let text = spec("%{_datadir}/x/a.conf \\\n%changelog\n");
    let found = missing_dirs(&text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].text, "%{_datadir}/x");
    assert_eq!(
        found[0].line,
        text.split('\n').position(|l| l.contains("a.conf")).unwrap()
    );
}

#[test]
fn the_whitelist_is_sorted_and_free_of_the_jay_directories() {
    assert!(STANDARD_DIRS.windows(2).all(|w| w[0] < w[1]));
    for dir in [
        "/usr/share/wayland-sessions",
        "/usr/share/xdg-desktop-portal",
        "/usr/share/xdg-desktop-portal/portals",
    ] {
        assert!(!is_standard_dir(dir), "{dir}");
    }
    for dir in [
        "/usr/share/man/de/man1",
        "/usr/share/locale/de/LC_MESSAGES",
        "/usr/share/icons/hicolor/scalable/apps",
        "/usr/share/icons/hicolor/48x48/apps",
    ] {
        assert!(is_standard_dir(dir), "{dir}");
    }
    assert!(!is_standard_dir("/usr/share/icons/hicolor/48x48/x"));
}

#[test]
fn the_jay_files_section_gets_its_dir_lines_before_the_first_entry() {
    let before = spec(JAY_FILES);
    let after = own_new_dirs(&before);
    let section = |s: &str| s.split("%files\n").nth(1).unwrap().to_string();
    assert_eq!(
        section(&after),
        "%{_bindir}/jay\n\
         %dir %{_datadir}/wayland-sessions\n\
         %{_datadir}/wayland-sessions/jay.desktop\n\
         %dir %{_datadir}/xdg-desktop-portal\n\
         %dir %{_datadir}/xdg-desktop-portal/portals\n\
         %{_datadir}/xdg-desktop-portal/portals/jay.portal\n\
         %{_datadir}/xdg-desktop-portal/jay-portals.conf\n\n%changelog\n"
    );
    assert_eq!(
        after.split("%files\n").next(),
        before.split("%files\n").next()
    );
}

#[test]
fn a_second_run_adds_nothing() {
    let once = own_new_dirs(&spec(JAY_FILES));
    assert_eq!(own_new_dirs(&once), once);
}

#[test]
fn a_spec_that_needs_nothing_is_returned_as_it_is() {
    let text = spec("%{_bindir}/jay\n");
    assert_eq!(own_new_dirs(&text), text);
    assert_eq!(own_new_dirs(""), "");
}

#[test]
fn the_line_goes_inside_the_subpackage_and_if_block_of_its_entry() {
    let text = spec("%{_bindir}/jay\n").replace(
        "%changelog\n",
        "%files -n jay-extra\n%if 0%{?x}\n%attr(0644,root,root) %{_datadir}/extra/a.conf\n\
         %endif\n\n%changelog\n",
    );
    let after = own_new_dirs(&text);
    assert!(
        after.contains("%files -n jay-extra\n%if 0%{?x}\n%dir %{_datadir}/extra\n%attr(0644"),
        "{after}"
    );
}

#[test]
fn crlf_lines_get_crlf_dir_lines() {
    let text = spec(JAY_FILES).replace('\n', "\r\n");
    let after = own_new_dirs(&text);
    assert!(after.contains("%dir %{_datadir}/wayland-sessions\r\n%{_datadir}/wayland"));
    assert!(!after.replace("\r\n", "").contains('\n'));
    assert_eq!(own_new_dirs(&after), after);
}

#[test]
fn nothing_is_removed_or_reordered() {
    let before = spec(JAY_FILES);
    let after = own_new_dirs(&before);
    let mut rest = after.lines().peekable();
    for line in before.lines() {
        while rest.peek().is_some_and(|l| *l != line) {
            assert!(rest.next().unwrap().starts_with("%dir "));
        }
        assert_eq!(rest.next(), Some(line));
    }
}
