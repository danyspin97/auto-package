use super::*;
use crate::pipeline::snapshot::CheckoutSnapshot;
use std::fs;

const SPEC: &str = "Name: x\nVersion: 1\nBuildRequires: gcc\n\n%build\nmake\n\n%check\nmake check\n\n%install\nmake install\n";

/// Printing the test log in %check is diagnostic: nothing else changed.
#[test]
fn a_print_in_check_is_a_diagnostic_only_edit() {
    let after = SPEC.replace("make check\n", "make check\necho ---\ncat test-suite.log\n");
    assert!(diagnostic_only(SPEC, &after));
}

/// A flag or a dependency is a real change, even with a print beside it.
#[test]
fn a_print_with_a_real_change_is_not_diagnostic_only() {
    let after = SPEC
        .replace("make check\n", "make check\ncat test-suite.log\n")
        .replace(
            "BuildRequires: gcc\n",
            "BuildRequires: gcc\nBuildRequires: libfoo\n",
        );
    assert!(!diagnostic_only(SPEC, &after));
}

/// A print outside the build sections (in %prep) is not a build diagnostic.
#[test]
fn a_print_outside_the_build_sections_is_not_diagnostic_only() {
    let with_prep = SPEC.replace("%build\n", "%prep\necho hi\n\n%build\n");
    let after = with_prep.replace("echo hi\n", "echo hello\n");
    assert!(!diagnostic_only(&with_prep, &after));
}

/// No change at all is not a diagnostic round.
#[test]
fn an_unchanged_spec_is_not_diagnostic_only() {
    assert!(!diagnostic_only(SPEC, SPEC));
}

/// A removed print counts as a diagnostic edit too.
#[test]
fn removing_a_print_is_diagnostic_only() {
    let before = SPEC.replace("make check\n", "make check\necho ---\n");
    assert!(diagnostic_only(&before, SPEC));
}

/// The marker belongs to one run: another run's marker is no marker.
#[test]
fn the_marker_is_read_for_its_own_run_only() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!last_round_diagnostic(dir.path(), "run-a"));
    set_last_round_diagnostic(dir.path(), "run-a", true);
    assert!(last_round_diagnostic(dir.path(), "run-a"));
    assert!(!last_round_diagnostic(dir.path(), "run-b"));
    set_last_round_diagnostic(dir.path(), "run-a", false);
    assert!(!last_round_diagnostic(dir.path(), "run-a"));
}

/// A round that changed no file clears the marker, and a real change does too.
#[test]
fn only_a_second_print_round_is_refused() {
    assert_eq!(
        round_verdict(false, false, false, true),
        RoundVerdict::Clear
    );
    assert_eq!(round_verdict(true, false, false, true), RoundVerdict::Clear);
    assert_eq!(round_verdict(true, true, true, true), RoundVerdict::Clear);
    assert_eq!(round_verdict(true, true, false, false), RoundVerdict::Mark);
    assert_eq!(round_verdict(true, true, false, true), RoundVerdict::Refuse);
}

/// A real round between two print rounds means the next print round is first.
#[test]
fn a_real_round_between_prints_resets_the_count() {
    let mut marked = false;
    let mut verdicts = Vec::new();
    for (spec_changed, diag, other) in [
        (true, true, false),
        (false, false, true),
        (true, true, false),
    ] {
        let v = round_verdict(spec_changed, diag, other, marked);
        marked = v == RoundVerdict::Mark;
        verdicts.push(v);
    }
    assert_eq!(
        verdicts,
        [RoundVerdict::Mark, RoundVerdict::Clear, RoundVerdict::Mark]
    );
}

/// A patch edited in the same turn as a print makes the turn a real change.
#[test]
fn a_patch_changed_in_the_same_turn_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("x.spec"), "Name: x\n").unwrap();
    fs::write(dir.path().join("fix.patch"), "old\n").unwrap();
    let snap = CheckoutSnapshot::take(dir.path());
    assert!(!other_files_changed(dir.path(), &snap, "x.spec"));
    fs::write(dir.path().join("fix.patch"), "new\n").unwrap();
    assert!(other_files_changed(dir.path(), &snap, "x.spec"));
}

/// A new top-level file in the turn is a change too.
#[test]
fn a_new_file_in_the_turn_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("x.spec"), "Name: x\n").unwrap();
    let snap = CheckoutSnapshot::take(dir.path());
    fs::write(dir.path().join("new.changes"), "").unwrap();
    assert!(other_files_changed(dir.path(), &snap, "x.spec"));
}
