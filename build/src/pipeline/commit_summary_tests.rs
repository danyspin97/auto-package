//! The `osc ci` message never carries a flag or failure text.

use super::commit::*;
use super::commit_summary::*;

const LONG_FLAG: &str = "FLAG: a fix turn answered blocked: the cargo_vendor service cannot run because the host's rustup has no default toolchain";

fn changes(body: &str) -> String {
    let sep = "-".repeat(67);
    format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n{body}\n")
}

#[test]
fn a_flag_and_its_wrapped_lines_are_not_part_of_the_summary() {
    let wrapped = "  FLAG: a fix turn answered blocked: the cargo_vendor service\n    cannot run because the host's rustup has no default toolchain";
    let summary = changes_summary(
        &changes(&format!(
            "- Update to 1.15.0\n{wrapped}\n- Drop the patch\n"
        )),
        "",
    )
    .unwrap();
    assert_eq!(summary, "Update to 1.15.0; Drop the patch");
}

#[test]
fn an_unwrapped_flag_under_a_bullet_is_skipped() {
    let summary =
        changes_summary(&changes(&format!("- Update to 1.15.0\n  {LONG_FLAG}")), "").unwrap();
    assert_eq!(summary, "Update to 1.15.0");
}

#[test]
fn an_entry_with_only_flags_gives_the_default_message() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pkg.changes"),
        changes(&format!("  {LONG_FLAG}\n    more text")),
    )
    .unwrap();
    let baseline = tempfile::tempdir().unwrap();
    assert_eq!(commit_message(dir.path(), baseline.path()), "auto-package");
}

#[test]
fn a_bullet_that_opens_with_failure_text_is_refused() {
    for text in [
        "a fix turn answered blocked: no toolchain",
        "Agent blocked: no toolchain",
    ] {
        assert_eq!(changes_summary(&changes(&format!("- {text}")), ""), None);
    }
}

#[test]
fn legitimate_bullets_that_mention_failure_words_are_kept() {
    for text in [
        "Fix tests that cannot run because of X",
        "Document the FLAG: marker",
        "Stop the agent blocked state in the docs",
    ] {
        let summary = changes_summary(&changes(&format!("- {text}")), "").unwrap();
        assert_eq!(summary, text);
    }
}

#[test]
fn a_wrapped_bullet_with_a_flag_word_is_kept_whole() {
    // As the wrapper writes it: `FLAG:` ends the line, it never starts one.
    let bullet = format!("- {} FLAG:\n  marker and more", "x".repeat(62));
    let summary = changes_summary(&changes(&bullet), "").unwrap();
    assert!(summary.contains("FLAG: marker"), "{summary}");
}

#[test]
fn a_clean_entry_is_still_cut_at_100_characters_on_a_word_boundary() {
    let bullets: String = (0..30).map(|i| format!("- Change number {i}\n")).collect();
    let summary = changes_summary(&changes(&bullets), "").unwrap();
    assert!(summary.chars().count() <= 100, "{summary}");
    let joined: String = (0..30)
        .map(|i| format!("Change number {i}"))
        .collect::<Vec<_>>()
        .join("; ");
    let next = joined[summary.len()..].chars().next().unwrap();
    assert!(joined.starts_with(&summary) && next == ' ', "{summary}");
}
