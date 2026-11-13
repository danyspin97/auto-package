use super::branch_cleanup::clean_with_origin;
use super::*;

const ORIGIN: &str = "\
-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 2.6.0:
  * Support more templates.
";

/// A checkout of `pkg` at `version` whose newest `.changes` is `changes`.
fn checkout(version: &str, changes: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("pkg.spec"),
        format!("Name: pkg\nVersion: {version}\n"),
    )
    .unwrap();
    fs::write(dir.path().join("pkg.changes"), changes).unwrap();
    dir
}

fn changes_of(dir: &tempfile::TempDir) -> String {
    fs::read_to_string(dir.path().join("pkg.changes")).unwrap()
}

#[test]
fn a_dependency_bullet_of_an_unpublished_update_entry_is_dropped() {
    let dir = checkout(
        "2.7.1",
        "-------------------------------------------------------------------\n\
         Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>\n\
         \n\
         - Update to 2.7.1\n\
         \x20 * Add a template option.\n\
         - Require binaryornot 0.4.4 and requests 2.23.0 as the new release declares\n",
    );
    let messages = clean_with_origin(dir.path(), Some(ORIGIN), "pkg", &[]);
    assert_eq!(messages.len(), 1, "{messages:?}");
    let text = changes_of(&dir);
    assert!(!text.contains("Require binaryornot"), "{text}");
    assert!(text.contains("* Add a template option."), "{text}");
}

#[test]
fn a_placeholder_bullet_of_an_unpublished_update_entry_is_dropped() {
    let dir = checkout(
        "2.7.1",
        "-------------------------------------------------------------------\n\
         Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>\n\
         \n\
         - Update to 2.7.1\n\
         \x20 * No upstream changelog found or provided\n",
    );
    clean_with_origin(dir.path(), Some(ORIGIN), "pkg", &[]);
    let text = changes_of(&dir);
    assert!(!text.contains("No upstream changelog"), "{text}");
    assert!(text.contains("- Update to 2.7.1"), "{text}");
}

#[test]
fn an_origin_entry_is_never_edited() {
    let published = ORIGIN.replace(
        "* Support more templates.",
        "* Support more templates.\n- Require pytest",
    );
    let dir = checkout("2.6.0", &published);
    let messages = clean_with_origin(dir.path(), Some(&published), "pkg", &[]);
    assert!(messages.is_empty(), "{messages:?}");
    assert_eq!(changes_of(&dir), published);
}

#[test]
fn no_origin_leaves_the_changes_alone() {
    let dir = checkout(
        "2.7.1",
        "-------------------------------------------------------------------\n\
         Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>\n\
         \n\
         - Update to 2.7.1\n\
         - Require pytest\n",
    );
    assert!(clean_with_origin(dir.path(), None, "pkg", &[]).is_empty());
    assert!(changes_of(&dir).contains("- Require pytest"));
}

#[test]
fn a_service_source_dir_the_spec_does_not_name_is_removed() {
    let dir = checkout("2.7.1", ORIGIN);
    fs::write(
        dir.path().join("_service"),
        r#"<services><service name="cargo_vendor" mode="manual"><param name="src">pleamar</param></service></services>"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("pleamar/src")).unwrap();
    let messages = clean_with_origin(dir.path(), Some(ORIGIN), "pkg", &[]);
    assert_eq!(
        messages,
        vec!["deleted 'pleamar/' from the checkout: a service left it there".to_string()]
    );
    assert!(!dir.path().join("pleamar").exists());
}

#[test]
fn a_service_source_dir_the_expanded_spec_names_is_kept() {
    let dir = checkout("2.7.1", ORIGIN);
    fs::write(
        dir.path().join("_service"),
        r#"<services><service name="cargo_vendor" mode="manual"><param name="src">pleamar</param></service></services>"#,
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("pleamar")).unwrap();
    let named = vec!["pleamar".to_string()];
    assert!(clean_with_origin(dir.path(), Some(ORIGIN), "pkg", &named).is_empty());
    assert!(dir.path().join("pleamar").exists());
}
