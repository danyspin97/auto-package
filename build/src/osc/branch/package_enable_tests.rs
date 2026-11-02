use super::package_flags::*;

const PORT: &str = "openSUSE_Factory_zSystems";

const META: &str = "<package name=\"p\">\n  <title>T</title>\n</package>\n";

#[test]
fn a_package_without_build_flags_gets_a_block_with_the_enable() {
    let edited = with_repositories_enabled(META, &[PORT]).unwrap().unwrap();
    assert_eq!(
        edited,
        "<package name=\"p\">\n  <title>T</title>\n  <build>\n    <enable repository=\"openSUSE_Factory_zSystems\"/>\n  </build>\n</package>\n"
    );
}

#[test]
fn an_enable_joins_the_flags_a_package_has() {
    let xml =
        "<package name=\"p\">\n  <build>\n    <disable arch=\"armv7l\"/>\n  </build>\n</package>\n";
    let edited = with_repositories_enabled(xml, &[PORT]).unwrap().unwrap();
    assert_eq!(
        edited,
        "<package name=\"p\">\n  <build>\n    <disable arch=\"armv7l\"/>\n    <enable repository=\"openSUSE_Factory_zSystems\"/>\n  </build>\n</package>\n"
    );
}

#[test]
fn a_self_closing_build_block_is_opened_for_the_enable() {
    let xml = "<package name=\"p\">\n  <build/>\n</package>\n";
    let edited = with_repositories_enabled(xml, &[PORT]).unwrap().unwrap();
    assert!(
        edited.contains(
            "  <build>\n    <enable repository=\"openSUSE_Factory_zSystems\"/>\n  </build>"
        )
    );
}

#[test]
fn a_repository_already_enabled_changes_nothing() {
    let once = with_repositories_enabled(META, &[PORT]).unwrap().unwrap();
    assert_eq!(with_repositories_enabled(&once, &[PORT]).unwrap(), None);
}

#[test]
fn an_enable_after_the_origin_copy_survives_and_the_copy_undoes_only_itself() {
    let origin =
        "<package name=\"p\">\n  <build>\n    <disable arch=\"armv7l\"/>\n  </build>\n</package>\n";
    let enabled = with_repositories_enabled(origin, &[PORT]).unwrap().unwrap();
    // A copy of the origin's flags replaces the build block, enable and all.
    let copied = match merge_build_flags(origin, &enabled).unwrap() {
        FlagMerge::Changed { xml, .. } => xml,
        other => panic!("the copy changes the enable away: {other:?}"),
    };
    assert!(!copied.contains("<enable"));
    // Enabling again, after the copy, restores it exactly once.
    let again = with_repositories_enabled(&copied, &[PORT])
        .unwrap()
        .unwrap();
    assert_eq!(
        again
            .matches("<enable repository=\"openSUSE_Factory_zSystems\"/>")
            .count(),
        1
    );
    assert_eq!(with_repositories_enabled(&again, &[PORT]).unwrap(), None);
}

#[test]
fn a_meta_that_is_not_a_package_is_refused() {
    assert!(with_repositories_enabled("<project name=\"p\"></project>", &[PORT]).is_err());
}
