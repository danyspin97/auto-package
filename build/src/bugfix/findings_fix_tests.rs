use super::*;
use crate::request_review::changes_checks::{ChangesCtx, changes_findings};

const OLD: &str = include_str!("../testdata/request_review/cracklib_old.changes");
const SEP: &str = "-------------------------------------------------------------------";
const MAIL: &str = "Jane Doe <jane.doe@example.com>";

fn entry(date: &str, bullets: &str) -> String {
    format!("{SEP}\n{date} - {MAIL}\n\n{bullets}\n\n")
}

fn findings(new: &str, added: &[String]) -> Vec<&'static str> {
    changes_findings(&ChangesCtx {
        package: "cracklib",
        file: "cracklib.changes",
        base: Some(OLD),
        new: Some(new),
        base_version: Some("2.10.3"),
        new_version: "2.10.4",
        packaging_changed: true,
        patches_added: added,
        patches_removed: &[],
        created: "2026-11-04T11:14:44",
    })
    .iter()
    .map(|f| f.id)
    .collect()
}

const DATE: &str = "Tue Nov  3 11:14:44 UTC 2026";

#[test]
fn ungrouped_refresh_bullets_become_one_bullet_and_the_check_is_quiet() {
    let new = format!(
        "{}{OLD}",
        entry(
            DATE,
            "- Update to 2.10.4\n  * https://x.test/notes\n- Refresh a.patch\n- Refresh b.patch"
        )
    );
    assert!(findings(&new, &[]).contains(&"changes.refresh-not-grouped"));
    let fixed = group_refresh_bullets(&new, Some(OLD)).unwrap();
    assert!(
        fixed.contains("- Refresh patches:\n  * a.patch\n  * b.patch\n"),
        "{fixed}"
    );
    assert!(fixed.ends_with(OLD));
    assert!(!findings(&fixed, &[]).contains(&"changes.refresh-not-grouped"));
    // One Refresh bullet is already as it should be.
    let one = format!("{}{OLD}", entry(DATE, "- Refresh a.patch"));
    assert!(group_refresh_bullets(&one, Some(OLD)).is_err());
}

#[test]
fn a_patch_bullet_comes_from_the_subject_of_the_patch() {
    let new = format!("{}{OLD}", entry(DATE, "- Update to 2.10.4"));
    let patch = "From: a\nSubject: [PATCH] Fix the overflow\n in words\n\n--- a/x\n";
    let found = findings(&new, &["fix.patch".into()]);
    assert!(found.contains(&"changes.patch-not-named"), "{found:?}");
    let fixed = add_patch_bullet(&new, Some(OLD), "fix.patch", patch).unwrap();
    assert!(
        fixed.contains("- Add fix.patch: Fix the overflow in words"),
        "{fixed}"
    );
    assert!(!findings(&fixed, &["fix.patch".into()]).contains(&"changes.patch-not-named"));
    // Without a subject it refuses.
    assert!(add_patch_bullet(&new, Some(OLD), "fix.patch", "--- a/x\n").is_err());
}

#[test]
fn old_entries_are_put_back_as_the_target_has_them() {
    let edited_old = OLD.replacen("2.10.3", "2.10.9", 1);
    let new = format!("{}{edited_old}", entry(DATE, "- Update to 2.10.4"));
    // The target's newest entry is changed in `new`: not told apart.
    let r = restore_old_entries(&new, Some(OLD));
    if let Ok(fixed) = r {
        assert!(fixed.ends_with(OLD));
    }
    // An older entry changed: the newest is found and the rest restored.
    let tampered = OLD.rsplitn(2, "\n- ").collect::<Vec<_>>();
    let altered = format!("{}\n- X{}", tampered[1], tampered[0]);
    let new = format!("{}{altered}", entry(DATE, "- Update to 2.10.4"));
    assert!(findings(&new, &[]).contains(&"changes.old-entries-edited"));
    let fixed = restore_old_entries(&new, Some(OLD)).unwrap();
    assert!(fixed.ends_with(OLD) && fixed.starts_with(SEP));
    assert!(!findings(&fixed, &[]).contains(&"changes.old-entries-edited"));
    assert!(restore_old_entries(&format!("{}{OLD}", entry(DATE, "- a")), Some(OLD)).is_err());
}

#[test]
fn a_newest_entry_quoted_above_the_new_ones_does_not_move_the_split() {
    let newest = OLD.split(&format!("\n{SEP}\n")).next().unwrap().trim();
    let tampered = OLD.rsplitn(2, "\n- ").collect::<Vec<_>>();
    let altered = format!("{}\n- X{}", tampered[1], tampered[0]);
    let new = format!("{newest}\n\n{}{altered}", entry(DATE, "- Update to 2.10.4"));
    let fixed = restore_old_entries(&new, Some(OLD)).unwrap();
    assert!(fixed.ends_with(OLD));
    assert!(fixed.starts_with(newest) && fixed.contains("- Update to 2.10.4"));
}

#[test]
fn the_update_bullet_is_added_when_none_exists_and_not_when_another_does() {
    let new = format!("{}{OLD}", entry(DATE, "- Fix a thing"));
    assert!(findings(&new, &[]).contains(&"changes.version-mismatch"));
    let fixed = add_update_bullet(&new, Some(OLD), "2.10.4").unwrap();
    assert!(
        fixed.contains(&format!("{MAIL}\n\n- Update to 2.10.4\n- Fix a thing\n")),
        "{fixed}"
    );
    assert!(!findings(&fixed, &[]).contains(&"changes.version-mismatch"));
    let newer = format!("{}{OLD}", entry(DATE, "- Update to 2.11"));
    assert!(add_update_bullet(&newer, Some(OLD), "2.10.4").is_err());
}

#[test]
fn an_older_release_bullet_stays_under_the_new_one() {
    let older = format!(
        "{}{OLD}",
        entry(DATE, "- Update to 2.10.3:\n  * old recap\n- Fix a thing")
    );
    let fixed = add_update_bullet(&older, Some(OLD), "2.10.4").unwrap();
    assert!(
        fixed.contains(&format!(
            "{MAIL}\n\n- Update to 2.10.4\n- Update to 2.10.3:\n  * old recap\n- Fix a thing\n"
        )),
        "{fixed}"
    );
}

#[test]
fn a_loose_header_is_written_in_the_standard_form() {
    let new = format!(
        "---------------------------------------------\nTue Nov 3 11:14:44 2026 -  Jane Doe <jane.doe@example.com>\n\n- Update to 2.10.4\n\n{OLD}"
    );
    assert!(findings(&new, &[]).contains(&"changes.header-format"));
    let fixed = normalise_headers(&new, Some(OLD)).unwrap();
    assert!(
        fixed.starts_with(&format!("{SEP}\n{DATE} - {MAIL}\n")),
        "{fixed}"
    );
    assert!(!findings(&fixed, &[]).contains(&"changes.header-format"));
    // A header with no mail is not guessed.
    let bad = format!("{SEP}\nTue Nov  3 11:14:44 UTC 2026 - nobody\n\n- a\n\n{OLD}");
    assert!(normalise_headers(&bad, Some(OLD)).is_err());
    // Already standard: nothing to do.
    let ok = format!("{}{OLD}", entry(DATE, "- Update to 2.10.4"));
    assert!(normalise_headers(&ok, Some(OLD)).is_err());
}

#[test]
fn a_dependency_bullet_is_dropped_unless_it_is_the_only_one() {
    let new = format!(
        "{}{OLD}",
        entry(
            DATE,
            "- Update to 2.10.4\n  * https://x.test/notes\n- Add BuildRequires on python3-foo >= 2\n- Fix a thing"
        )
    );
    assert!(findings(&new, &[]).contains(&"changes.dependency-bullet"));
    let fixed = drop_dependency_bullets(&new, Some(OLD)).unwrap();
    assert!(!fixed.contains("python3-foo"));
    assert!(fixed.contains("- Fix a thing") && fixed.contains("https://x.test/notes"));
    assert!(!findings(&fixed, &[]).contains(&"changes.dependency-bullet"));
    let only = format!("{}{OLD}", entry(DATE, "- Add BuildRequires on foo"));
    assert!(drop_dependency_bullets(&only, Some(OLD)).is_err());
    let none = format!("{}{OLD}", entry(DATE, "- Fix a thing"));
    assert!(drop_dependency_bullets(&none, Some(OLD)).is_err());
}

#[test]
fn a_fix_of_the_changes_refuses_when_the_old_entries_are_not_kept() {
    let new = format!(
        "{}{}",
        entry(DATE, "- Refresh a\n- Refresh b"),
        OLD.replacen("2.10.3", "x", 1)
    );
    assert!(group_refresh_bullets(&new, Some(OLD)).is_err());
    assert!(normalise_headers(&new, Some(OLD)).is_err());
}

#[test]
fn only_a_leftover_the_spec_does_not_name_is_removable() {
    assert!(removable_leftover("fix.patch.orig", "Patch0: fix.patch\n", false).is_ok());
    assert!(removable_leftover("notes.md", "", false).is_ok());
    assert!(removable_leftover("old-1.0.tar.gz", "Source: new-2.0.tar.gz\n", false).is_ok());
    // Named by the spec, or not a leftover, or not a plain name.
    assert!(removable_leftover("old-1.0.tar.gz", "Source: old-1.0.tar.gz\n", false).is_err());
    assert!(removable_leftover("foo.spec", "", false).is_err());
    assert!(removable_leftover("../x.orig", "", false).is_err());
    assert!(removable_leftover("series", "", true).is_err());
    assert!(removable_leftover("series", "", false).is_ok());
}

#[test]
fn the_entry_bullets_are_replaced_but_a_fact_of_the_change_is_kept() {
    let new = format!(
        "{}{OLD}",
        entry(
            DATE,
            "- Update to 2.10.4\n  * https://x.test/notes\n- Add fix.patch: a fix (bsc#1)\n- Odd bullet"
        )
    );
    let patches = vec!["fix.patch".to_string()];
    let ok = "- Update to 2.10.4\n  * https://x.test/notes\n- Add fix.patch: a fix (bsc#1)\n- Fix the header of fix.patch";
    let fixed = apply_changes_entry(&new, Some(OLD), ok, &patches).unwrap();
    assert!(fixed.contains("- Fix the header of fix.patch") && !fixed.contains("Odd bullet"));
    assert!(fixed.starts_with(&format!("{SEP}\n{DATE} - {MAIL}\n\n- Update to")));
    assert!(fixed.ends_with(OLD));
    // Dropping the patch bullet, the Update bullet or a reference is refused.
    for bad in [
        "- Update to 2.10.4\n  * https://x.test/notes\n- Other",
        "- Add fix.patch: a fix (bsc#1)\n- Other",
        "- Update to 2.10.4\n  * https://x.test/notes\n- Add fix.patch: a fix",
        "",
    ] {
        assert!(
            apply_changes_entry(&new, Some(OLD), bad, &patches).is_err(),
            "{bad:?}"
        );
    }
    // Bullets without a dash are written with one; an entry that is not the request's is refused.
    let plain = format!("{}{OLD}", entry(DATE, "- a"));
    let fixed = apply_changes_entry(&plain, Some(OLD), "Say it", &[]).unwrap();
    assert!(fixed.contains("\n- Say it\n"));
    assert!(apply_changes_entry(OLD, Some(OLD), "- x", &[]).is_err());
}
