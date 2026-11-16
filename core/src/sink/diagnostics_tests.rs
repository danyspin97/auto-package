use super::*;
use std::path::Path;

#[test]
fn an_identical_line_is_offered_once() {
    let mut d = Diagnostics::default();
    assert_eq!(
        d.offer("warning: a", false),
        Offer::Print("warning: a".into())
    );
    assert_eq!(d.offer("warning: a", false), Offer::Repeat);
    assert_eq!(d.offer("warning: a", true), Offer::Repeat);
    assert_eq!(
        d.offer("warning: b", false),
        Offer::Print("warning: b".into())
    );
}

#[test]
fn lines_are_held_while_the_tui_owns_the_terminal() {
    let mut d = Diagnostics::default();
    assert_eq!(d.offer("note: x", true), Offer::Held);
    assert_eq!(d.offer("note: y", true), Offer::Held);
    assert_eq!(d.held, vec!["note: x".to_string(), "note: y".to_string()]);
}

#[test]
fn a_debug_line_is_recorded_once_per_key_and_never_printed_under_the_tui() {
    let mut d = Diagnostics {
        debug: true,
        ..Diagnostics::default()
    };
    assert_eq!(
        d.offer_debug("scrub:TOK", "scrub TOK", true),
        Offer::Recorded
    );
    assert_eq!(
        d.offer_debug("scrub:TOK", "scrub TOK", false),
        Offer::Repeat
    );
    assert_eq!(
        d.offer_debug("scrub:KEY", "scrub KEY", false),
        Offer::Print("scrub KEY".into())
    );
}

#[test]
fn a_debug_line_is_not_printed_below_debug_level() {
    let mut d = Diagnostics::default();
    assert_eq!(
        d.offer_debug("scrub:TOK", "scrub TOK", false),
        Offer::Recorded
    );
    assert!(!debug_prints(false, false));
    assert!(!debug_prints(true, true));
    assert!(debug_prints(true, false));
}

#[test]
fn the_held_summary_is_one_line_and_counts_messages() {
    assert_eq!(held_line(0, None), None);
    assert_eq!(
        held_line(1, Some(Path::new("/w/run/diagnostics.log"))).as_deref(),
        Some("1 message held while the TUI was open, see /w/run/diagnostics.log")
    );
    assert_eq!(
        held_line(3, None).as_deref(),
        Some("3 messages held while the TUI was open")
    );
}

#[test]
fn lines_are_appended_to_the_diagnostics_file() {
    let dir = std::env::temp_dir().join(format!("ap-diag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("diagnostics.log");
    let _ = std::fs::remove_file(&file);
    let mut d = Diagnostics {
        file: Some(file.clone()),
        ..Diagnostics::default()
    };
    d.offer("warning: kept", true);
    d.offer("warning: kept", true);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text, "warning: kept\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lines_recorded_before_the_file_is_attached_are_written_to_it() {
    let dir = std::env::temp_dir().join(format!("ap-diag-early-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("diagnostics.log");
    let _ = std::fs::remove_file(&file);
    let mut d = Diagnostics::default();
    d.offer("warning: early", false);
    d.offer("note: early too", false);
    assert!(!file.exists());
    d.attach_file(&file);
    d.offer("warning: later", true);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text, "warning: early\nnote: early too\nwarning: later\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_warning_and_its_held_line_are_one_line() {
    assert_eq!(canonical("warning: x"), "x");
    assert_eq!(canonical("note: x"), "note: x");
    let mut d = Diagnostics::default();
    assert_eq!(d.offer("warning: x", true), Offer::Held);
    // The reporter's copy of the same warning is not held twice.
    assert_eq!(d.offer(canonical("warning: x"), true), Offer::Repeat);
    assert_eq!(d.held.len(), 1);
}

#[test]
fn a_held_line_keeps_the_severity_it_was_written_with() {
    let warning = |line: &str| matches!(held_event(line), Event::Warning { .. });
    assert!(warning("warning: x"));
    assert!(warning("  warning: the claude stream had no result"));
    assert!(!warning("  checking openSUSE:Factory for foo"));
    assert!(!warning("note: x"));
    match held_event("  searching GitLab for: foo") {
        Event::Progress { message, .. } => assert_eq!(message, "  searching GitLab for: foo"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_held_line_that_reports_a_failure_stays_a_warning() {
    let warning = |line: &str| matches!(held_event(line), Event::Warning { .. });
    assert!(warning("✗ agent lookup failed to run"));
    assert!(warning("  cannot move the spec aside"));
    assert!(warning("  could not record the archive digest"));
    assert!(warning("  quilt pop -a failed"));
    assert!(warning("  Quilt push FAILED on the second patch"));
    assert!(warning("  can't open the changes file"));
}

#[test]
fn a_routine_held_line_is_a_progress_line() {
    let progress = |line: &str| matches!(held_event(line), Event::Progress { .. });
    assert!(progress("  looking up the agent for foo"));
    assert!(progress("  found 3 candidates for foo"));
    assert!(progress("  quilt push -a"));
}
