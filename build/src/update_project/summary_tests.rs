use super::*;
use unicode_width::UnicodeWidthStr;

fn item(pkg: &str, old: &str, new: &str) -> Item {
    Item {
        pkg: pkg.into(),
        origin: "Base:System".into(),
        ecosystem: Ecosystem::GitRepo {
            url: "https://github.com/a/b".into(),
        },
        old_version: old.into(),
        new_version: new.into(),
        upstream_tag: None,
        held_version: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: Vec::new(),
    }
}

fn updated(pkg: &str, old: &str, new: &str) -> ItemResult {
    ItemResult {
        item: item(pkg, old, new),
        outcome: ItemOutcome::Updated,
        sr: SrOutcome::NotRequested,
    }
}

fn summary(results: &[ItemResult], width: usize) -> String {
    summary_text("b", results, &[], &[], &[], false, width)
}

/// The display column at which `needle` starts in each line that holds it.
fn columns_of(text: &str, needle: &str) -> Vec<usize> {
    text.lines()
        .filter_map(|l| l.find(needle).map(|at| UnicodeWidthStr::width(&l[..at])))
        .collect()
}

#[test]
fn an_empty_summary_has_no_section_headings() {
    let text = summary(&[], 100);
    assert!(text.contains("update summary\n"), "{text}");
    assert!(!text.contains("(0)"), "{text}");
    assert!(
        text.lines().all(|l| !l.ends_with("):")),
        "a heading with no entries: {text}"
    );
}

#[test]
fn only_the_sections_with_entries_are_listed() {
    let text = summary(&[updated("jay", "1.0", "1.1")], 100);
    assert!(text.contains("Updated (1):\n  jay 1.0 → 1.1\n"), "{text}");
    for heading in ["Failed", "Skipped", "Not started", "Needs the maintainer"] {
        assert!(!text.contains(heading), "{heading} listed: {text}");
    }
}

#[test]
fn the_version_arrows_line_up_by_display_width() {
    let results = [
        updated("jay", "1.0", "1.1"),
        updated("字字", "10", "11"),
        updated("e\u{301}x", "1", "2"),
        updated("ab", "1.0-rc1", "1.0"),
    ];
    let text = summary(&results, 100);
    let cols = columns_of(&text, " → ");
    assert_eq!(cols.len(), 4, "{text}");
    assert!(cols.iter().all(|c| *c == cols[0]), "{text}");
}

#[test]
fn a_package_name_wider_than_the_others_is_not_cut() {
    let text = summary(
        &[updated("a-very-long-package-name", "1.0.0-rc1", "2.0")],
        20,
    );
    assert!(
        text.contains("  a-very-long-package-name 1.0.0-rc1 → 2.0\n"),
        "{text}"
    );
}

#[test]
fn the_old_version_column_is_capped_by_the_width() {
    // Narrow: the padding of the old version stops at the width, the row is
    // still whole and its new version is still shown.
    let results = [
        updated("jay", "1.0", "1.1"),
        updated("kay", "1.0.0-beta-long", "2"),
    ];
    let text = summary(&results, 16);
    assert!(text.contains("  jay 1.0     → 1.1\n"), "{text}");
    assert!(text.contains("  kay 1.0.0-beta-long → 2\n"), "{text}");
}

#[test]
fn the_summary_text_carries_no_escape_sequence() {
    let results = [
        updated("jay", "1.0", "1.1"),
        ItemResult {
            item: item("b", "1", "2"),
            outcome: ItemOutcome::Failed("boom".into()),
            sr: SrOutcome::NotRequested,
        },
    ];
    let text = summary_text("home:u:b", &results, &[], &[], &[], true, 100);
    assert!(!text.contains('\u{1b}'), "{text}");
}
