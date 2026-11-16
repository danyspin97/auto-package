use super::changes_checks::{ChangesCtx, changes_findings};
use super::{Finding, Severity, ids};

const OLD: &str = include_str!("../testdata/request_review/cracklib_old.changes");
const NEW: &str = include_str!("../testdata/request_review/cracklib_new.changes");

const SEP: &str = "-------------------------------------------------------------------";

fn ctx<'a>(base: Option<&'a str>, new: Option<&'a str>) -> ChangesCtx<'a> {
    ChangesCtx {
        package: "cracklib",
        file: "cracklib.changes",
        base,
        new,
        base_version: Some("2.10.3"),
        new_version: "2.10.4",
        packaging_changed: true,
        patches_added: &[],
        patches_removed: &[],
        created: "2026-11-03T11:14:44",
    }
}

fn ids_of(f: &[Finding]) -> Vec<&'static str> {
    f.iter().map(|f| f.id).collect()
}

/// An entry on top of the old text, dated before the request.
fn with_entry(bullets: &str) -> String {
    format!(
        "{SEP}\nMon Nov  2 16:03:24 UTC 2026 - Jane Doe <jane.doe@example.com>\n\n{bullets}\n\n{OLD}"
    )
}

#[test]
fn the_real_request_entry_gives_no_finding() {
    let found = changes_findings(&ctx(Some(OLD), Some(NEW)));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_changed_spec_without_a_new_entry_is_a_blocker() {
    let found = changes_findings(&ctx(Some(OLD), Some(OLD)));
    assert_eq!(ids_of(&found), [ids::CHANGES_MISSING_ENTRY]);
    assert_eq!(found[0].severity, Severity::Blocker);
    let mut quiet = ctx(Some(OLD), Some(OLD));
    quiet.packaging_changed = false;
    assert!(changes_findings(&quiet).is_empty());
}

#[test]
fn edited_old_entries_are_a_warning() {
    let new = with_entry("- Update to 2.10.4\n  * Fixed things");
    let base_changed = OLD.replacen("2.10.3", "2.10.2", 1);
    let found = changes_findings(&ctx(Some(&base_changed), Some(&new)));
    assert!(
        ids_of(&found).contains(&ids::CHANGES_OLD_ENTRIES_EDITED),
        "{found:?}"
    );
}

#[test]
fn a_header_that_is_not_the_format_is_a_warning() {
    let bad = format!("{SEP}\nNov 2 2026 - me\n\n- Update to 2.10.4\n  * notes\n\n{OLD}");
    let found = changes_findings(&ctx(Some(OLD), Some(&bad)));
    assert!(
        ids_of(&found).contains(&ids::CHANGES_HEADER_FORMAT),
        "{found:?}"
    );
    let short = NEW.replacen(SEP, "----------------------", 1);
    let found = changes_findings(&ctx(Some(OLD), Some(&short)));
    assert!(
        ids_of(&found).contains(&ids::CHANGES_HEADER_FORMAT),
        "{found:?}"
    );
}

#[test]
fn an_entry_older_than_the_previous_or_after_the_request_is_a_warning() {
    let old = with_entry("- Update to 2.10.4\n  * notes")
        .replace("Nov  2 16:03:24 UTC 2026", "Jan  2 16:03:24 UTC 2020");
    let found = changes_findings(&ctx(Some(OLD), Some(&old)));
    assert!(
        ids_of(&found).contains(&ids::CHANGES_DATE_ORDER),
        "{found:?}"
    );
    let late = with_entry("- Update to 2.10.4\n  * notes")
        .replace("Nov  2 16:03:24 UTC 2026", "Dec 25 16:03:24 UTC 2026");
    let found = changes_findings(&ctx(Some(OLD), Some(&late)));
    assert!(
        ids_of(&found).contains(&ids::CHANGES_DATE_ORDER),
        "{found:?}"
    );
}

#[test]
fn a_version_change_needs_a_matching_update_bullet() {
    let none = with_entry("- Refresh foo.patch");
    let found = changes_findings(&ctx(Some(OLD), Some(&none)));
    let m = found
        .iter()
        .find(|f| f.id == ids::CHANGES_VERSION_MISMATCH)
        .unwrap();
    assert_eq!(m.severity, Severity::Warning);
    // A bullet that names another release is clearly wrong.
    let other = with_entry("- Update to 2.9.0\n  * notes");
    let found = changes_findings(&ctx(Some(OLD), Some(&other)));
    let m = found
        .iter()
        .find(|f| f.id == ids::CHANGES_VERSION_MISMATCH)
        .unwrap();
    assert_eq!(m.severity, Severity::Blocker);
}

#[test]
fn the_rc_spellings_of_one_version_are_one_release() {
    for (a, b) in [
        ("3.0rc1", "3.0~rc1"),
        ("3.0rc1", "3.0.rc.1"),
        ("3.0-rc1", "3.0.rc1"),
        ("1.0a", "1.0.a"),
    ] {
        assert_eq!(
            crate::request_review::changes_checks::mismatch(a, b),
            None,
            "{a} vs {b}"
        );
    }
    use crate::request_review::changes_checks::mismatch;
    assert_eq!(mismatch("3.0rc1", "3.0"), Some(false));
    assert_eq!(mismatch("3.0rc1", "3.1"), Some(true));
}

#[test]
fn an_update_bullet_that_is_no_version_or_that_rpm_spells_differently_is_no_blocker() {
    let run = |bullet: &str, new_version: &'static str| {
        let new = with_entry(&format!("- {bullet}\n  * notes"));
        let mut c = ctx(Some(OLD), Some(&new));
        c.new_version = new_version;
        changes_findings(&c)
    };
    for (bullet, version) in [
        ("Update to git snapshot 20240101", "1.0+git20240101"),
        ("Update to the latest upstream release", "2.10.4"),
        ("Update to 1.0-rc1", "1.0~rc1"),
        ("Update to v2.10.4", "2.10.4"),
        ("Update to 1.0+git20240101", "1.0+git20240101.abc"),
        ("Update to 2.10.4:", "2.10.4"),
    ] {
        let found = run(bullet, version);
        assert!(
            found.iter().all(|f| f.id != ids::CHANGES_VERSION_MISMATCH),
            "{bullet} vs {version}: {found:?}"
        );
    }
    // Shorter or longer than the Version: a slip, not a blocker.
    let found = run("Update to 2.10", "2.10.4");
    let m = found
        .iter()
        .find(|f| f.id == ids::CHANGES_VERSION_MISMATCH)
        .unwrap();
    assert_eq!(m.severity, Severity::Warning);
    let found = run("Update to 3.0", "2.10.4");
    let m = found
        .iter()
        .find(|f| f.id == ids::CHANGES_VERSION_MISMATCH)
        .unwrap();
    assert_eq!(m.severity, Severity::Blocker);
}

#[test]
fn an_update_bullet_without_a_version_change_is_a_warning() {
    let new = with_entry("- Update to 2.10.4\n  * notes");
    let mut same = ctx(Some(OLD), Some(&new));
    same.base_version = Some("2.10.4");
    let found = changes_findings(&same);
    assert_eq!(ids_of(&found), [ids::CHANGES_UPDATE_WITHOUT_VERSION_CHANGE]);
}

#[test]
fn an_update_without_notes_or_a_link_is_a_warning_with_the_risk_text() {
    let bare = with_entry("- Update to 2.10.4");
    let found = changes_findings(&ctx(Some(OLD), Some(&bare)));
    let n = found
        .iter()
        .find(|f| f.id == ids::CHANGES_NO_UPSTREAM_NOTES)
        .unwrap();
    assert!(n.message.contains("no upstream changes listed for 2.10.4"));
    let linked = with_entry("- Update to 2.10.4\n  https://example.org/news");
    assert!(changes_findings(&ctx(Some(OLD), Some(&linked))).is_empty());
}

#[test]
fn a_dependency_bullet_is_a_warning_quoting_it() {
    let e = with_entry(
        "- Update to 2.10.4\n  * notes\n- Add gdb and dwz BuildRequires, which configure needs",
    );
    let found = changes_findings(&ctx(Some(OLD), Some(&e)));
    let d = found
        .iter()
        .find(|f| f.id == ids::CHANGES_DEPENDENCY_BULLET)
        .unwrap();
    assert!(d.message.contains("Add gdb and dwz BuildRequires"));
    let lower = with_entry("- Update to 2.10.4\n  * notes\n- Add requires on libfoo");
    let found = changes_findings(&ctx(Some(OLD), Some(&lower)));
    assert!(
        found.iter().any(|f| f.id == ids::CHANGES_DEPENDENCY_BULLET),
        "{found:?}"
    );
    let fine = with_entry("- Update to 2.10.4\n  * notes\n- Fix the build with GCC 15");
    assert!(changes_findings(&ctx(Some(OLD), Some(&fine))).is_empty());
}

fn housekeeping_findings(found: &[Finding]) -> Vec<&Finding> {
    found
        .iter()
        .filter(|f| f.id == ids::CHANGES_HOUSEKEEPING_BULLET)
        .collect()
}

#[test]
fn a_housekeeping_bullet_is_a_warning_quoting_it_with_or_without_an_update() {
    let bare = with_entry(
        "- Fix the source archive name to the lowercase name PyPI uses for this release",
    );
    let found = changes_findings(&ctx(Some(OLD), Some(&bare)));
    let h = housekeeping_findings(&found);
    assert_eq!(h.len(), 1, "{found:?}");
    assert_eq!(h[0].severity, Severity::Warning);
    assert!(
        h[0].message
            .contains("Fix the source archive name to the lowercase")
    );
    let with_update =
        with_entry("- Update to 2.10.4\n  * notes\n- Drop the mock import workaround in %check");
    let found = changes_findings(&ctx(Some(OLD), Some(&with_update)));
    let h = housekeeping_findings(&found);
    assert_eq!(h.len(), 1, "{found:?}");
    assert!(
        h[0].message
            .contains("Drop the mock import workaround in %check")
    );
}

#[test]
fn a_housekeeping_bullet_of_a_published_entry_is_not_flagged() {
    let published = format!(
        "{SEP}\nMon Nov  2 16:03:24 UTC 2026 - Jane Doe <jane.doe@example.com>\n\n- Update to 2.10.4\n  * notes\n- Remove the sed workaround in %prep\n\n{OLD}"
    );
    let found = changes_findings(&ctx(Some(&published), Some(&published)));
    assert!(housekeeping_findings(&found).is_empty(), "{found:?}");
}

#[test]
fn a_non_housekeeping_bullet_is_not_flagged_as_housekeeping() {
    let e = with_entry(
        "- Update to 2.10.4\n  * notes\n- Fix the source archive host to files.pythonhosted.org",
    );
    let found = changes_findings(&ctx(Some(OLD), Some(&e)));
    assert!(housekeeping_findings(&found).is_empty(), "{found:?}");
}

#[test]
fn two_refresh_bullets_give_the_grouped_form() {
    let e = with_entry("- Update to 2.10.4\n  * notes\n- Refresh a.patch\n- Refresh b.patch");
    let found = changes_findings(&ctx(Some(OLD), Some(&e)));
    let g = found
        .iter()
        .find(|f| f.id == ids::CHANGES_REFRESH_NOT_GROUPED)
        .unwrap();
    assert!(
        g.message.contains("Refresh patches: * a.patch * b.patch"),
        "{}",
        g.message
    );
    let one = with_entry("- Update to 2.10.4\n  * notes\n- Refresh a.patch");
    assert!(changes_findings(&ctx(Some(OLD), Some(&one))).is_empty());
}

#[test]
fn a_patch_added_or_removed_must_be_named() {
    let e = with_entry("- Update to 2.10.4\n  * notes\n- Add new.patch to fix the build");
    let added = ["new.patch".to_string(), "other.patch".to_string()];
    let removed = ["gone.patch".to_string()];
    let mut c = ctx(Some(OLD), Some(&e));
    c.patches_added = &added;
    c.patches_removed = &removed;
    let found = changes_findings(&c);
    let msgs: Vec<_> = found
        .iter()
        .filter(|f| f.id == ids::CHANGES_PATCH_NOT_NAMED)
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(msgs.len(), 2, "{found:?}");
    assert!(msgs.iter().any(|m| m.contains("other.patch")));
    assert!(msgs.iter().any(|m| m.contains("gone.patch")));
}

#[test]
fn todo_a_macro_or_the_package_name_in_a_bullet_is_a_note() {
    for bullet in ["Fix TODO", "Use %{version} here", "Fix cracklib crash"] {
        let e = with_entry(&format!("- Update to 2.10.4\n  * notes\n- {bullet}"));
        let found = changes_findings(&ctx(Some(OLD), Some(&e)));
        let f = found.iter().find(|f| f.id == ids::CHANGES_FORBIDDEN_TEXT);
        assert_eq!(f.map(|f| f.severity), Some(Severity::Note), "{bullet}");
    }
    let e = with_entry("- Update to 2.10.4\n  * Fixed cracklib bug\n- Fix a crash");
    assert!(changes_findings(&ctx(Some(OLD), Some(&e))).is_empty());
}

#[test]
fn a_require_bullet_without_a_dependency_word_is_a_dependency_bullet() {
    use super::changes_checks::is_dependency_bullet;
    assert!(is_dependency_bullet(
        "Require binaryornot 0.4.4 and requests 2.23.0 as the new release declares"
    ));
    assert!(!is_dependency_bullet(
        "Fix the install path of the man pages"
    ));
}

/// A jump from 1.13.0 to 1.14.1 over the old text.
fn jump() -> ChangesCtx<'static> {
    let mut c = ctx(Some(OLD), None);
    c.base_version = Some("1.13.0");
    c.new_version = "1.14.1";
    c
}

#[test]
fn the_older_release_bullets_of_a_jump_are_recap_not_a_mismatch() {
    let text = with_entry("- Update to 1.14.1:\n  * new\n\n- Update to 1.14.0:\n  * old");
    let mut c = jump();
    c.new = Some(&text);
    let found = changes_findings(&c);
    assert!(
        !ids_of(&found).contains(&ids::CHANGES_VERSION_MISMATCH),
        "{found:?}"
    );
    assert!(
        !ids_of(&found).contains(&ids::CHANGES_NO_UPSTREAM_NOTES),
        "{found:?}"
    );
}

#[test]
fn only_the_newest_bullet_of_an_entry_needs_notes() {
    let bare = with_entry("- Update to 1.14.1:\n- Update to 1.14.0:");
    let mut c = jump();
    c.new = Some(&bare);
    let found = changes_findings(&c);
    let notes: Vec<_> = found
        .iter()
        .filter(|f| f.id == ids::CHANGES_NO_UPSTREAM_NOTES)
        .collect();
    assert_eq!(notes.len(), 1, "{found:?}");
    assert!(notes[0].message.contains("1.14.1"), "{found:?}");
}

#[test]
fn a_recap_bullet_outside_the_jump_is_a_mismatch() {
    // 1.15.0 is newer than the Version, 1.12.0 older than the base.
    for stray in ["1.15.0", "1.12.0"] {
        let text = with_entry(&format!(
            "- Update to 1.14.1:\n  * new\n\n- Update to {stray}:\n  * old"
        ));
        let mut c = jump();
        c.new = Some(&text);
        let found = changes_findings(&c);
        let m = found
            .iter()
            .find(|f| f.id == ids::CHANGES_VERSION_MISMATCH)
            .unwrap_or_else(|| panic!("{stray}: {found:?}"));
        assert_eq!(m.severity, Severity::Blocker, "{stray}");
    }
}

/// The two housekeeping bullets convention 40 gives no bullet to are recognised,
/// and a bullet that names a host, a project or a skipped test is not.
#[test]
fn housekeeping_bullets_are_recognised_and_others_are_not() {
    use super::changes_checks::is_housekeeping_bullet;
    assert!(is_housekeeping_bullet(
        "Fix the source archive name to the lowercase name PyPI uses for this release"
    ));
    assert!(is_housekeeping_bullet(
        "Use the lowercase archive name PyPI uses for this release"
    ));
    assert!(is_housekeeping_bullet(
        "Drop the mock import workaround in %check, which the tests no longer need"
    ));
    assert!(is_housekeeping_bullet("Remove the sed workaround in %prep"));
    // A host, a project or a skipped test is a change, whatever its verb.
    assert!(!is_housekeeping_bullet(
        "Fix the source archive host to files.pythonhosted.org"
    ));
    assert!(!is_housekeeping_bullet(
        "Fix the source archive name to a new project"
    ));
    assert!(!is_housekeeping_bullet(
        "Drop the workaround that skipped test_foo in %check"
    ));
    assert!(!is_housekeeping_bullet(
        "Drop the workaround in %check and deselect test_foo"
    ));
    assert!(!is_housekeeping_bullet(
        "Add a deselect for test_foo in %check"
    ));
    assert!(!is_housekeeping_bullet("Update to 2.0.0"));
    assert!(!is_housekeeping_bullet(
        "Drop the workaround for the old build"
    ));
}

/// The old entries are the last copy of their newest entry in the file, so a new
/// entry that quotes that text is not where they start.
#[test]
fn a_new_entry_quoting_the_old_newest_entry_is_not_cut_at_the_quote() {
    let base = OLD.replacen("2.10.3", "2.10.2", 1);
    let newest = base.split(&format!("\n{SEP}\n")).next().unwrap().trim();
    let new = format!(
        "{SEP}\nMon Nov  2 16:03:24 UTC 2026 - Jane Doe <jane.doe@example.com>\n\n\
         - Quote the old entry:\n{newest}\n- Update to 2.10.4\n  * Fixed things\n\n{base}\n"
    );
    let found = changes_findings(&ctx(Some(&base), Some(&new)));
    assert!(
        !ids_of(&found).contains(&ids::CHANGES_VERSION_MISMATCH),
        "the Update bullet below the quote is in the new entry: {found:?}"
    );
    assert!(
        ids_of(&found).contains(&ids::CHANGES_OLD_ENTRIES_EDITED),
        "{found:?}"
    );
}
