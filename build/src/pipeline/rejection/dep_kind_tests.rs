use super::dep_kind::{DepKind, judged_kind_changes, kind_changes};

const ORIGIN: &str = "Name: river\nVersion: 1.0\nRequires:  python-Babel >= 2.9\nBuildRequires:  python-setuptools\n";

fn names(changes: &[super::dep_kind::KindChange]) -> Vec<String> {
    changes.iter().map(|c| c.describe()).collect()
}

#[test]
fn a_requires_moved_to_recommends_is_a_kind_change() {
    // The old-binary review turn: `Requires: python-Babel` became `Recommends:`.
    let after = "Name: river\nVersion: 1.0\nRecommends:  python-Babel >= 2.9\nBuildRequires:  python-setuptools\n";
    let changes = kind_changes(ORIGIN, ORIGIN, after);
    assert_eq!(
        names(&changes),
        vec!["Requires: python-Babel -> Recommends: python-Babel"]
    );
    assert_eq!(changes[0].from, DepKind::Requires);
    assert_eq!(changes[0].to, DepKind::Recommends);
}

#[test]
fn an_unchanged_inherited_dependency_is_not_a_kind_change() {
    assert!(kind_changes(ORIGIN, ORIGIN, ORIGIN).is_empty());
}

#[test]
fn a_new_dependency_line_is_not_judged() {
    // A Recommends for a name the origin does not declare is new, not inherited.
    let after = format!("{ORIGIN}Recommends:  python-tqdm\n");
    assert!(kind_changes(ORIGIN, ORIGIN, &after).is_empty());
    // Nor is a Requires added next to the origin's own kind for the same name.
    let both = format!("{ORIGIN}Recommends:  python-Babel\n");
    assert!(kind_changes(ORIGIN, ORIGIN, &both).is_empty());
}

#[test]
fn a_kind_that_was_already_changed_before_the_turn_is_not_this_edits() {
    // The turn started from a spec that already had the Recommends: the move is
    // not this turn's, so the gate does not judge it here.
    let before = "Name: river\nVersion: 1.0\nRecommends:  python-Babel >= 2.9\nBuildRequires:  python-setuptools\n";
    assert!(kind_changes(ORIGIN, before, before).is_empty());
}

#[test]
fn a_dependency_dropped_outright_is_not_a_kind_change() {
    let after = "Name: river\nVersion: 1.0\nBuildRequires:  python-setuptools\n";
    assert!(kind_changes(ORIGIN, ORIGIN, after).is_empty());
}

#[test]
fn the_name_matches_across_python_prefix_and_case() {
    let after =
        "Name: river\nVersion: 1.0\nRecommends:  python-babel\nBuildRequires:  python-setuptools\n";
    assert_eq!(kind_changes(ORIGIN, ORIGIN, after).len(), 1);
}

#[test]
fn a_build_requires_moved_to_requires_is_a_kind_change() {
    let after =
        "Name: river\nVersion: 1.0\nRequires:  python-Babel >= 2.9\nRequires:  python-setuptools\n";
    let changes = kind_changes(ORIGIN, ORIGIN, after);
    assert_eq!(
        names(&changes),
        vec!["BuildRequires: python-setuptools -> Requires: python-setuptools"]
    );
}

#[test]
fn a_forced_change_is_allowed() {
    let after = "Name: river\nVersion: 1.0\nRecommends:  python-Babel >= 2.9\nBuildRequires:  python-setuptools\n";
    let forced = vec!["python-babel".to_string()];
    assert!(judged_kind_changes(ORIGIN, ORIGIN, after, &forced, false).is_empty());
    // A force on another dependency does not allow this one.
    let other = vec!["python-tqdm".to_string()];
    assert_eq!(
        judged_kind_changes(ORIGIN, ORIGIN, after, &other, false).len(),
        1
    );
}

#[test]
fn the_fix_scope_may_change_an_inherited_kind() {
    let after = "Name: river\nVersion: 1.0\nRecommends:  python-Babel >= 2.9\nBuildRequires:  python-setuptools\n";
    assert!(judged_kind_changes(ORIGIN, ORIGIN, after, &[], true).is_empty());
    assert_eq!(
        judged_kind_changes(ORIGIN, ORIGIN, after, &[], false).len(),
        1
    );
}

#[test]
fn a_conditional_or_macro_requires_is_not_judged() {
    let origin = "Requires:  (python-foo if python3)\nRequires:  %{python_module bar}\n";
    let after = "Recommends:  (python-foo if python3)\nRecommends:  %{python_module bar}\n";
    assert!(kind_changes(origin, origin, after).is_empty());
}
