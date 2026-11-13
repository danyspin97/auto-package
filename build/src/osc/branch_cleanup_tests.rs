use super::branch_cleanup::{clean_with_origin, without_explained_flags};
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
    let messages = clean_with_origin(dir.path(), Some(ORIGIN), None, "pkg", &[], false, &|_| {});
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
    clean_with_origin(dir.path(), Some(ORIGIN), None, "pkg", &[], false, &|_| {});
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
    let messages = clean_with_origin(
        dir.path(),
        Some(&published),
        None,
        "pkg",
        &[],
        false,
        &|_| {},
    );
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
    assert!(clean_with_origin(dir.path(), None, None, "pkg", &[], false, &|_| {}).is_empty());
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
    let messages = clean_with_origin(dir.path(), Some(ORIGIN), None, "pkg", &[], false, &|_| {});
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
    assert!(
        clean_with_origin(
            dir.path(),
            Some(ORIGIN),
            None,
            "pkg",
            &named,
            false,
            &|_| {}
        )
        .is_empty()
    );
    assert!(dir.path().join("pleamar").exists());
}

/// The cookiecutter case: the origin's spec already skips the test, and the
/// branch's spec only moves the version on.
const COOKIE_ORIGIN_CHANGES: &str = "\
-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 2.6.0:
  * Support more templates.
";

const COOKIE_ORIGIN_SPEC: &str = "\
Name: cookiecutter
Version: 2.6.0

%check
%pytest -k \"not test_generate_file_verbose_template_syntax_error\"
";

const COOKIE_SKIP: &str = "-k \"not test_generate_file_verbose_template_syntax_error\"";

fn unpublished_with_flag(flag: &str) -> String {
    format!(
        "-------------------------------------------------------------------\n\
         Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>\n\
         \n\
         - Update to 2.7.1\n\
         \x20 * Add a template option.\n\
         \x20 FLAG: tests skipped: {flag}\n"
    )
}

#[test]
fn a_flag_the_origin_spec_already_explains_is_dropped() {
    let spec = COOKIE_ORIGIN_SPEC.replace("2.6.0", "2.7.1");
    let changes = unpublished_with_flag(COOKIE_SKIP);
    let new = without_explained_flags(&changes, COOKIE_ORIGIN_CHANGES, COOKIE_ORIGIN_SPEC, &spec)
        .expect("the stale flag goes");
    assert!(!new.contains("FLAG:"), "{new}");
    assert!(new.contains("* Add a template option."), "{new}");
    assert!(new.ends_with("* Add a template option.\n"), "{new:?}");
}

#[test]
fn a_skip_the_origin_spec_lacks_is_still_flagged() {
    let spec = format!(
        "{}%pytest -k \"not test_slow\"\n",
        COOKIE_ORIGIN_SPEC.replace("2.6.0", "2.7.1")
    );
    let changes = unpublished_with_flag(COOKIE_SKIP);
    let new = without_explained_flags(&changes, COOKIE_ORIGIN_CHANGES, COOKIE_ORIGIN_SPEC, &spec)
        .expect("the new skip is kept");
    assert!(
        new.contains("FLAG: tests skipped: -k \"not test_slow\""),
        "{new}"
    );
    assert!(!new.contains("test_generate_file_verbose"), "{new}");
}

#[test]
fn a_flag_in_a_published_entry_is_never_edited() {
    let spec = COOKIE_ORIGIN_SPEC.replace("2.6.0", "2.6.0");
    let published = format!(
        "{}  FLAG: tests skipped: {COOKIE_SKIP}\n",
        COOKIE_ORIGIN_CHANGES
    );
    assert!(without_explained_flags(&published, &published, COOKIE_ORIGIN_SPEC, &spec).is_none());
}

#[test]
fn a_blocked_flag_is_kept_whatever_the_spec_says() {
    let spec = COOKIE_ORIGIN_SPEC.replace("2.6.0", "2.7.1");
    let changes = format!(
        "{}  FLAG: a fix turn answered blocked: no network\n",
        unpublished_with_flag(COOKIE_SKIP)
    );
    let new = without_explained_flags(&changes, COOKIE_ORIGIN_CHANGES, COOKIE_ORIGIN_SPEC, &spec)
        .expect("the stale skip goes");
    assert!(
        new.contains("FLAG: a fix turn answered blocked: no network"),
        "{new}"
    );
    assert!(!new.contains("tests skipped"), "{new}");
}

#[test]
fn the_flag_is_dropped_from_the_checkout_file() {
    let dir = checkout("2.7.1", &unpublished_with_flag(COOKIE_SKIP));
    fs::write(
        dir.path().join("pkg.spec"),
        COOKIE_ORIGIN_SPEC.replace("2.6.0", "2.7.1"),
    )
    .unwrap();
    let messages = clean_with_origin(
        dir.path(),
        Some(COOKIE_ORIGIN_CHANGES),
        Some(COOKIE_ORIGIN_SPEC),
        "pkg",
        &[],
        false,
        &|_| {},
    );
    assert!(messages.iter().any(|m| m.contains("FLAG")), "{messages:?}");
    assert!(!changes_of(&dir).contains("FLAG:"));
}

/// An unpublished update entry with a bare `Update to` bullet, and an origin
/// that packages 2.6.0 with its own entry. The spec names no forge and the
/// checkout holds no archive, so the recap has nowhere to be read from.
#[test]
fn a_missing_recap_nobody_can_read_is_reported_and_the_entry_stays() {
    let bare = "-------------------------------------------------------------------\n\
         Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>\n\
         \n\
         - Update to 2.7.1\n\
         \n\
         -------------------------------------------------------------------\n\
         Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>\n\
         \n\
         - Update to 2.6.0:\n\
         \x20 * Support more templates.\n";
    let dir = checkout("2.7.1", bare);
    let origin_spec = "Name: pkg\nVersion: 2.6.0\n";
    let origin = ORIGIN.to_string();
    let warnings = std::cell::RefCell::new(Vec::new());
    let messages = clean_with_origin(
        dir.path(),
        Some(&origin),
        Some(origin_spec),
        "pkg",
        &[],
        true,
        &|m| warnings.borrow_mut().push(m),
    );
    assert!(messages.is_empty(), "{messages:?}");
    assert_eq!(changes_of(&dir), bare);
    assert_eq!(warnings.borrow().len(), 1, "{:?}", warnings.borrow());
    assert!(
        warnings.borrow()[0].contains("no upstream notes available for 2.6.0..2.7.1"),
        "{:?}",
        warnings.borrow()
    );
}
