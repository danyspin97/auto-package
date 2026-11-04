use std::cell::RefCell;

use super::*;
use std::path::Path;

const SEP: &str = "-------------------------------------------------------------------";
const BULLET: &str = "- Add foo-CVE-2025-1234.patch: check the length (bsc#1234567, CVE-2025-1234)";
const FILE: &str = "foo-CVE-2025-1234.patch";

fn old() -> String {
    format!("{SEP}\nMon Jan  1 00:00:00 UTC 2026 - a@b.c\n\n- Update to 1.0\n\n")
}

fn with_entry(bullets: &str) -> String {
    format!(
        "{SEP}\nTue Jan  2 00:00:00 UTC 2026 - a@b.c\n\n{bullets}\n\n{}",
        old()
    )
}

#[test]
fn without_an_entry_of_this_run_the_bullet_opens_one() {
    assert_eq!(
        fix_bullet_edit(&old(), &old(), FILE, BULLET),
        BulletEdit::NewEntry
    );
    assert_eq!(
        fix_bullet_edit("", &old(), FILE, BULLET),
        BulletEdit::NewEntry
    );
}

#[test]
fn the_agents_bullet_that_names_the_file_is_replaced() {
    let cur = with_entry(&format!("- Add {FILE}: my words\n- Fix the build"));
    let BulletEdit::Rewritten(text) = fix_bullet_edit(&cur, &old(), FILE, BULLET) else {
        panic!("not rewritten");
    };
    assert!(
        text.contains(&format!("{BULLET}\n- Fix the build")),
        "{text}"
    );
    assert!(!text.contains("my words"));
    // The older entry is untouched.
    assert!(text.contains("- Update to 1.0"));
}

#[test]
fn another_bullet_is_kept_and_the_fix_bullet_goes_first() {
    let cur = with_entry("- Fix the build");
    let BulletEdit::Rewritten(text) = fix_bullet_edit(&cur, &old(), FILE, BULLET) else {
        panic!("not rewritten");
    };
    assert!(
        text.contains(&format!("{BULLET}\n- Fix the build")),
        "{text}"
    );
}

#[test]
fn the_bullet_already_there_is_unchanged() {
    let cur = with_entry(BULLET);
    assert_eq!(
        fix_bullet_edit(&cur, &old(), FILE, BULLET),
        BulletEdit::Unchanged
    );
}

#[test]
fn set_fix_bullet_opens_an_entry_or_rewrites_the_file() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("foo.changes");
    std::fs::write(&path, old()).unwrap();
    let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let add = |_: &Path, msg: &str| {
        asked.borrow_mut().push(msg.to_string());
        Ok(())
    };
    set_fix_bullet(d.path(), &old(), FILE, BULLET, &add).unwrap();
    assert_eq!(asked.borrow().len(), 1);
    assert!(asked.borrow()[0].starts_with("Add foo-CVE"));
    std::fs::write(&path, with_entry(&format!("- Add {FILE}: mine"))).unwrap();
    set_fix_bullet(d.path(), &old(), FILE, BULLET, &add).unwrap();
    assert_eq!(asked.borrow().len(), 1, "no new entry");
    assert!(std::fs::read_to_string(&path).unwrap().contains(BULLET));
}
