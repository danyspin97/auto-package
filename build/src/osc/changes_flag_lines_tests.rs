//! Flags of a `.changes` entry that the wrapper broke over several lines.

const SEP: &str = "-------------------------------------------------------------------\n";
const HEADER: &str = "Mon Nov  2 16:38:10 UTC 2026 - A <a@b.c>";

fn entry(header: &str, body: &str) -> String {
    format!("{SEP}{header}\n\n{body}\n")
}

const LONG_FLAG: &str = "FLAG: a fix turn answered blocked: the cargo_vendor service cannot run because the host's rustup has no default toolchain";

/// `body` with every line wrapped as the file is when it is written.
fn wrapped(body: &str) -> String {
    body.lines()
        .flat_map(super::changes::wrap_changes_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn flagged(body: &str, flags: &[&str]) -> String {
    reflagged(&entry(HEADER, body), flags)
}

fn reflagged(text: &str, flags: &[&str]) -> String {
    let flags: Vec<String> = flags.iter().map(|f| f.to_string()).collect();
    super::changes::with_flag_lines_in_release_entry(text, "1.0", &flags).unwrap()
}

#[test]
fn a_wrapped_flag_is_replaced_whole_and_leaves_no_orphan_line() {
    let first = wrapped(&format!("- Update to 1.0\n  {LONG_FLAG}\n- Real bullet"));
    assert!(first.lines().count() > 3, "{first}");
    let out = flagged(&first, &["FLAG: tests skipped: a"]);
    assert!(
        out.contains("- Update to 1.0\n  FLAG: tests skipped: a\n- Real bullet\n"),
        "{out}"
    );
    assert!(
        !out.contains("toolchain") && !out.contains("rustup"),
        "{out}"
    );
}

#[test]
fn flags_set_twice_or_changed_leave_one_copy_and_keep_the_bullet() {
    let body = wrapped(&format!("- Update to 1.0\n  {LONG_FLAG}"));
    let once = flagged(&body, &[LONG_FLAG]);
    let twice = reflagged(&wrapped(&once), &[LONG_FLAG]);
    assert_eq!(twice.matches("FLAG:").count(), 1, "{twice}");
    assert_eq!(twice.matches("toolchain").count(), 1, "{twice}");
    let other = reflagged(&wrapped(&twice), &["FLAG: check removed"]);
    assert!(
        other.contains("- Update to 1.0\n  FLAG: check removed\n"),
        "{other}"
    );
    assert!(!other.contains("toolchain"), "{other}");
}

#[test]
fn a_flag_continuation_is_indented_deeper_than_the_flag() {
    let lines = super::changes::wrap_changes_line(&format!("  {LONG_FLAG}"));
    assert!(lines.len() > 1);
    assert!(
        lines[1..].iter().all(|l| l.starts_with("    ")),
        "{lines:?}"
    );
}

#[test]
fn an_indented_bullet_or_blank_line_ends_a_flag() {
    let lines = ["  FLAG: x", "    more", "  * recap", "", "    text"];
    let mask = super::flag_line_mask(&lines);
    assert_eq!(mask, vec![true, true, false, false, false]);
}

/// An ordinary bullet whose `FLAG:` word falls where the wrapper breaks.
fn bullet_with_flag_word() -> String {
    format!("- {} FLAG: marker and more words after it", "x".repeat(62))
}

#[test]
fn a_wrapped_bullet_never_starts_a_line_with_flag() {
    let lines = super::changes::wrap_changes_line(&bullet_with_flag_word());
    assert!(lines.len() > 1, "{lines:?}");
    assert!(
        lines.iter().all(|l| !l.trim_start().starts_with("FLAG: ")),
        "{lines:?}"
    );
}

#[test]
fn a_wrapped_bullet_with_a_flag_word_survives_set_flag_lines() {
    let body = wrapped(&format!(
        "- Update to 1.0\n{}\n- Next",
        bullet_with_flag_word()
    ));
    let out = flagged(&body, &["FLAG: tests skipped: a"]);
    assert!(
        out.contains("marker and more words after it\n- Next"),
        "{out}"
    );
    let again = reflagged(&wrapped(&out), &["FLAG: check removed"]);
    assert!(
        again.contains("marker and more words after it\n- Next"),
        "{again}"
    );
    assert_eq!(again.matches("FLAG:").count(), 2, "{again}");
}
