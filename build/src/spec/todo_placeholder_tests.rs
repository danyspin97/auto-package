use super::todo_placeholders;

#[test]
fn finds_markers_outside_the_changelog_only() {
    let spec = "Summary: TODO: fill in summary\n%install\n# TODO: install\n%changelog\n- TODO in history\n";
    let found = todo_placeholders(spec);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].0, 1);
    assert_eq!(found[1].1, "# TODO: install");
}

#[test]
fn a_finished_spec_has_none() {
    assert!(todo_placeholders("Summary: A tool\n%build\n%make_build\n").is_empty());
}

#[test]
fn a_file_named_todo_in_doc_is_not_a_placeholder() {
    let spec = "%files\n%doc AUTHORS ChangeLog NEWS README THANKS TODO\n%license COPYING\n";
    assert!(todo_placeholders(spec).is_empty());
    let spec = "%doc README TODO NEWS\n%install\n%doc TODO\n";
    assert!(todo_placeholders(spec).is_empty());
}

#[test]
fn a_file_named_todo_in_files_or_a_path_is_not_a_placeholder() {
    let spec = "%files\n/usr/share/doc/foo/TODO\n%{_docdir}/foo/TODO.txt\n%install\n";
    assert!(todo_placeholders(spec).is_empty());
}

#[test]
fn a_license_placeholder_is_flagged() {
    let found = todo_placeholders("Name: foo\nLicense: TODO\n%doc README TODO\n");
    assert_eq!(found, vec![(2, "License: TODO".to_string())]);
}

#[test]
fn a_comment_in_files_is_still_searched() {
    let found = todo_placeholders("%files\n# TODO: list the libraries\n/usr/lib/foo.so\n");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, 2);
}
