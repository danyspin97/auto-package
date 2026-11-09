use crate::pipeline::carried_origin::carried_mode;
use crate::pipeline::update_mode::{newly_reported, split_inherited, style_problems};

/// A summary-only description and no `%license`, as a maintainer ships it.
const SUMMARY_ONLY: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: A tool for things\n\
    License: MIT\n\n%description\nA tool for things\n\n%build\n\n%files\n%{_bindir}/foo\n\n\
    %changelog\n";

/// The same recipe at the next version, with the same description.
const SUMMARY_ONLY_BUMPED: &str = "Name: foo\nVersion: 2.0\nRelease: 0\nSummary: A tool for things\n\
    License: MIT\n\n%description\nA tool for things\n\n%build\n\n%files\n%{_bindir}/foo\n\n\
    %changelog\n";

/// A described recipe with a `%license`, as a clean origin ships it.
const DESCRIBED: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: A tool for things\n\
    License: MIT\n\n%description\nA tool for things. It runs the jobs of a build.\n\n\
    %build\n\n%files\n%license LICENSE\n%{_bindir}/foo\n\n%changelog\n";

fn source_with_licence() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("LICENSE"), "MIT\n").unwrap();
    dir
}

#[test]
fn a_bumped_dependency_keeps_the_problems_its_origin_already_has() {
    let src = source_with_licence();
    let mode = carried_mode(Some(SUMMARY_ONLY.to_string()), true, false).expect("carried");
    let origin = style_problems(mode.origin_spec.as_deref().unwrap(), src.path());
    assert_eq!(origin.len(), 2, "the origin's description and %license");
    let (introduced, inherited) =
        split_inherited(style_problems(SUMMARY_ONLY_BUMPED, src.path()), &origin);
    assert!(introduced.is_empty(), "{introduced:?}");
    assert_eq!(inherited.len(), 2);
}

#[test]
fn a_new_package_has_no_origin_and_its_problems_are_introduced() {
    let src = source_with_licence();
    assert!(carried_mode(None, true, false).is_none());
    let (introduced, inherited) = split_inherited(style_problems(SUMMARY_ONLY, src.path()), &[]);
    assert_eq!(introduced.len(), 2);
    assert!(inherited.is_empty());
}

#[test]
fn a_problem_the_origin_does_not_have_is_fixed_in_a_bumped_dependency() {
    let src = source_with_licence();
    let mode = carried_mode(Some(DESCRIBED.to_string()), true, false).expect("carried");
    let origin = style_problems(mode.origin_spec.as_deref().unwrap(), src.path());
    assert!(origin.is_empty(), "the origin is clean: {origin:?}");
    let (introduced, inherited) =
        split_inherited(style_problems(SUMMARY_ONLY_BUMPED, src.path()), &origin);
    assert_eq!(introduced.len(), 2);
    assert!(inherited.is_empty());
}

#[test]
fn a_root_update_keeps_its_own_mode_and_a_regenerated_spec_is_not_carried() {
    assert!(carried_mode(Some(DESCRIBED.to_string()), true, true).is_none());
    assert!(carried_mode(Some(DESCRIBED.to_string()), false, false).is_none());
}

#[test]
fn an_inherited_problem_of_a_dependency_is_reported_once() {
    let mut mode = carried_mode(Some(SUMMARY_ONLY.to_string()), true, false).expect("carried");
    let first = newly_reported(&mut mode.reported, vec!["a".into()]);
    assert_eq!(first, vec!["a".to_string()]);
    assert!(newly_reported(&mut mode.reported, vec!["a".into()]).is_empty());
}
