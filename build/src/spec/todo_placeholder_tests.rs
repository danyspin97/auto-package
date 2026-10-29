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
