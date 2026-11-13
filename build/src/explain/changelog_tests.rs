use super::*;

/// Two entries, the newest first. The nested bullets are indented under the
/// entry's bullet, as OBS packages write them.
const CHANGES: &str = r#"-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - a@b.c

- Update to 0.7.12
  * Fix the parser for nested lists
    which spans two lines
  * Drop the old shim

-------------------------------------------------------------------
Old

- Old entry
"#;

#[test]
fn the_newest_entry_keeps_the_indentation_of_its_lines() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("p.changes");
    std::fs::write(&path, CHANGES).unwrap();
    let entry = latest_changes_entry(&path);
    assert_eq!(
        entry,
        vec![
            "Mon Oct 26 10:00:00 UTC 2026 - a@b.c".to_string(),
            "- Update to 0.7.12".to_string(),
            "  * Fix the parser for nested lists".to_string(),
            "    which spans two lines".to_string(),
            "  * Drop the old shim".to_string(),
        ]
    );
}

#[test]
fn the_changelog_section_prints_the_nested_bullets_indented() {
    let mut ex = Explanation::default();
    ex.outcome.changelog = latest_changes_entry_from(CHANGES);
    let text = ansi(&render(&ex, 80), false);
    let lines: Vec<&str> = text.lines().collect();
    let at = |needle: &str| {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .copied()
            .unwrap_or_else(|| panic!("no line with {needle:?} in\n{text}"))
    };
    let lead = |l: &str| l.len() - l.trim_start().len();
    let head = at("- Update to 0.7.12");
    let nested = at("* Fix the parser");
    let wrapped = at("which spans two lines");
    let next = at("* Drop the old shim");
    assert!(lead(nested) > lead(head), "{head:?} / {nested:?}");
    assert_eq!(lead(nested), lead(next));
    assert_eq!(lead(wrapped), lead(nested) + 2, "{wrapped:?}");
}

fn latest_changes_entry_from(text: &str) -> Vec<String> {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("p.changes");
    std::fs::write(&path, text).unwrap();
    latest_changes_entry(&path)
}
