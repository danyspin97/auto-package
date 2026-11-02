use crate::pipeline::build_rounds::BuildRounds;

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A fake fix-after-build loop: each script entry is the error lines of one
/// failed build. A failure that gets a fix turn is one round; the loop stops
/// at the first refused round. Returns the fix turns paid and the reason.
fn run_loop(script: &[Vec<String>], cap: usize, same: usize) -> (usize, Option<String>) {
    let mut rounds = BuildRounds::default();
    for errors in script {
        if let Err(reason) = rounds.next_round_with(errors, cap, same) {
            return (rounds.given, Some(reason));
        }
    }
    (rounds.given, None)
}

#[test]
fn the_same_errors_three_times_end_the_package_with_the_reason() {
    let e = lines(&["foo.c:10:10: fatal error: bar.hpp: No such file or directory"]);
    let script = vec![e.clone(), e.clone(), e.clone(), e.clone()];
    let (turns, reason) = run_loop(&script, 6, 3);
    assert_eq!(
        turns, 2,
        "the third build with the same errors gets no turn"
    );
    let reason = reason.expect("the loop must stop");
    assert!(
        reason.starts_with("the same build errors came back 3 times: foo.c:10:10"),
        "{reason}"
    );
}

#[test]
fn a_different_error_line_resets_the_repeat_count() {
    let a = lines(&["error: A"]);
    let b = lines(&["error: B"]);
    let script = vec![
        a.clone(),
        a.clone(),
        b.clone(),
        a.clone(),
        a.clone(),
        b.clone(),
    ];
    let (turns, reason) = run_loop(&script, 6, 3);
    assert_eq!(turns, 6);
    assert_eq!(reason, None);
}

#[test]
fn the_order_and_repeats_of_error_lines_do_not_change_the_key() {
    let first = lines(&["error: X", "error: Y"]);
    let second = lines(&["error: Y", "error: X", "error: Y"]);
    let script = vec![first, second.clone(), second];
    let (turns, reason) = run_loop(&script, 6, 3);
    assert_eq!(turns, 2);
    assert!(reason.unwrap().contains("came back 3 times"));
}

#[test]
fn a_build_with_no_error_line_is_never_a_repeat() {
    let script: Vec<Vec<String>> = (0..7).map(|_| lines(&["  ", ""])).collect();
    let (turns, reason) = run_loop(&script, 6, 3);
    assert_eq!(turns, 6);
    assert_eq!(reason.unwrap(), "fix-after-build round limit 6 reached");
}

#[test]
fn the_default_round_cap_is_six() {
    assert_eq!(ap_core::limits::DEFAULT_FIX_BUILD_ROUNDS, 6);
    assert_eq!(ap_core::limits::SAME_BUILD_ERRORS_LIMIT, 3);
    // Six different failures each get a turn; the seventh ends the package.
    let script: Vec<Vec<String>> = (0..7).map(|i| lines(&[&format!("error: {i}")])).collect();
    let mut rounds = BuildRounds::default();
    for errors in &script[..6] {
        assert!(rounds.next_round(errors).is_ok());
    }
    assert_eq!(rounds.given, 6);
    let reason = rounds.next_round(&script[6]).unwrap_err();
    assert_eq!(reason, "fix-after-build round limit 6 reached");
}

#[test]
fn a_restored_count_is_counted_against_the_cap() {
    let mut rounds = BuildRounds::default();
    rounds.given = 5;
    assert!(rounds.next_round(&lines(&["error: a"])).is_ok());
    assert!(rounds.next_round(&lines(&["error: b"])).is_err());
}

/// The fake loop with the default limits: a package whose builds keep failing
/// on the same error is stopped by the repeat limit, not by the round cap.
#[test]
fn a_stuck_package_stops_before_the_round_cap() {
    let e = lines(&["error: same"]);
    let script = vec![e.clone(); 10];
    let (turns, reason) = run_loop(
        &script,
        ap_core::limits::DEFAULT_FIX_BUILD_ROUNDS,
        ap_core::limits::SAME_BUILD_ERRORS_LIMIT,
    );
    assert_eq!(turns, 2);
    assert!(reason.unwrap().contains("came back 3 times"));
}
