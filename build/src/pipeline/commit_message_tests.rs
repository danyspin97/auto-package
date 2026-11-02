//! The `osc ci -m` message: one line, free of control characters, and bounded.

use super::commit::*;

fn changes(bullets: &str) -> String {
    let sep = "-".repeat(67);
    format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n{bullets}\n")
}

#[test]
fn a_summary_keeps_its_punctuation_and_drops_control_characters() {
    let working = changes("- Fixed\tthe\x1b[31m tab\n- Second *bold* @user 100% \"quoted\"\n");
    let summary = changes_summary(&working, "").unwrap();
    assert!(!summary.chars().any(char::is_control), "{summary:?}");
    for kept in ["100%", "*bold*", "@user", "\"quoted\""] {
        assert!(summary.contains(kept), "{kept} lost in {summary:?}");
    }
}

#[test]
fn a_long_summary_is_bounded() {
    let bullets: String = (0..60)
        .map(|i| format!("- Change number {i} with some words\n"))
        .collect();
    let summary = changes_summary(&changes(&bullets), "").unwrap();
    assert!(
        summary.chars().count() <= 100,
        "{}",
        summary.chars().count()
    );
}

#[test]
fn the_message_is_one_line() {
    assert_eq!(one_line("a\n\tb\r\nc"), "a b c");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pkg.changes"),
        changes("- A first\r\n  continued line\n- Second\n"),
    )
    .unwrap();
    let message = commit_message(dir.path());
    assert!(message.starts_with("auto-package: "), "{message}");
    assert!(
        !message.contains('\n') && !message.contains('\r'),
        "{message:?}"
    );
}
