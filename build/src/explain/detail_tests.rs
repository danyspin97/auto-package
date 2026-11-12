use super::detail::verbatim_lines;
use super::*;

fn failed(reason: &str, detail: Option<&str>) -> Explanation {
    Explanation {
        package: "foo".into(),
        outcome: Outcome {
            result: "failed".into(),
            detail: Some(reason.into()),
            failure_detail: detail.map(str::to_string),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The plain report of a work directory whose only run has `run_line` as its
/// `package_finished` and `state` as the package's state.json.
fn report_of(run_line: &serde_json::Value, state: &str) -> String {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let line = serde_json::to_string(run_line).unwrap();
    std::fs::create_dir_all(d.join("runs/R1")).unwrap();
    std::fs::write(d.join("runs/R1/run.jsonl"), format!("{line}\n")).unwrap();
    std::fs::create_dir_all(d.join("state/foo")).unwrap();
    std::fs::write(d.join("state/foo/state.json"), state).unwrap();
    let ex = explain(&Query {
        project_dir: d,
        package: "foo",
        run: None,
        package_notes: String::new(),
    })
    .unwrap();
    plain(&render(&ex, 100))
}

#[test]
fn the_detail_of_a_failure_is_printed_under_its_reason() {
    let ex = failed(
        "gave up",
        Some("round 1: osc failed\nround 2: osc failed again"),
    );
    let lines = render(&ex, 80);
    let text = plain(&lines);
    let reason = text.find("gave up").expect("the reason is shown");
    let first = text
        .find("    round 1: osc failed\n")
        .expect("indented detail");
    let second = text
        .find("    round 2: osc failed again\n")
        .expect("indented detail");
    assert!(reason < first && first < second, "{text}");
    assert!(!ansi(&lines, false).contains('\u{1b}'));
    assert!(ansi(&lines, true).contains("round 2: osc failed again"));
}

#[test]
fn no_detail_or_a_detail_equal_to_the_reason_adds_nothing() {
    let bare = plain(&render(&failed("gave up", None), 80));
    assert!(!bare.contains("round"), "{bare}");
    let same = plain(&render(&failed("gave up", Some("gave up")), 80));
    assert_eq!(same, bare);
}

#[test]
fn a_ladder_reason_from_state_and_the_detail_of_the_run_are_both_shown_once() {
    let run = serde_json::json!({
        "event": "package_finished",
        "pkg": "foo",
        "version": "2.0",
        "outcome": {"Failed": {
            "reason": "gave up",
            "disk_full": false,
            "note": {"detail": "round 1: osc failed\nround 2: osc failed again"}
        }}
    });
    let state = r#"{"last_outcome":"failed","last_failure_reason":"gave up","obs_name":"foo","ladder_reason":"osc keeps failing"}"#;
    let text = report_of(&run, state);
    assert_eq!(
        text.matches("round 2: osc failed again").count(),
        1,
        "{text}"
    );
    assert!(text.contains("osc keeps failing"), "{text}");
}

#[test]
fn an_indented_traceback_keeps_its_indentation() {
    let run = serde_json::json!({
        "event": "package_finished",
        "pkg": "foo",
        "version": "2.0",
        "outcome": {"Failed": {
            "reason": "tests failed",
            "disk_full": false,
            "note": {"detail": "Traceback (most recent call last):\n  File \"a.py\", line 3\n    raise ValueError\n\nValueError: boom"}
        }}
    });
    let text = report_of(&run, r#"{"obs_name":"foo"}"#);
    assert!(
        text.contains(
            "    Traceback (most recent call last):\n      File \"a.py\", line 3\n        raise ValueError\n\n    ValueError: boom\n"
        ),
        "{text}"
    );
}

#[test]
fn a_long_detail_line_is_cut_at_the_width_and_nothing_else_changes() {
    let long = "x".repeat(100);
    let lines = verbatim_lines(&long, 40);
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with("    ") && l.chars().count() <= 40)
    );
    let joined: String = lines.iter().map(|l| &l[4..]).collect();
    assert_eq!(joined, long);
    assert_eq!(verbatim_lines("a\n\n  b", 80), ["    a", "", "      b"]);
}

#[test]
fn the_detail_is_kept_out_of_the_json_report() {
    let json = serde_json::to_string(&failed("gave up", Some("round 1: osc failed"))).unwrap();
    assert!(!json.contains("round 1"), "{json}");
    assert!(!json.contains("failure_detail"), "{json}");
}

#[test]
fn a_failed_package_finished_event_carries_its_detail_to_the_outcome() {
    let mut p = Parsed::default();
    p.finished = Some(serde_json::json!({
        "event": "package_finished",
        "outcome": {"Failed": {
            "reason": "gave up",
            "disk_full": false,
            "note": {"detail": "round 1: /work/build failed"}
        }}
    }));
    let o = outcome(&p, None, Path::new("/work"), "foo", "foo", None);
    assert_eq!(o.detail.as_deref(), Some("gave up"));
    assert_eq!(
        o.failure_detail.as_deref(),
        Some("round 1: <work dir>/build failed")
    );

    p.finished = Some(serde_json::json!({
        "event": "package_finished",
        "outcome": {"Failed": {"reason": "gave up", "disk_full": false}}
    }));
    let o = outcome(&p, None, Path::new("/work"), "foo", "foo", None);
    assert_eq!(o.failure_detail, None);
}

#[test]
fn a_needs_maintainer_ladder_shows_its_reason_and_its_full_history() {
    let run = serde_json::json!({
        "event": "package_finished",
        "pkg": "foo",
        "version": "2.0",
        "outcome": {"NeedsMaintainer": {
            "reason": "foo could not be resolved: a security patch would be dropped.",
            "note": {"detail": "UNRESOLVED after the resolution ladder: foo: turn 0: a | round 1: b"}
        }}
    });
    // The memo is written for this kind too, but the report does not rely on it.
    let state = r#"{"last_outcome":"failed","obs_name":"foo"}"#;
    let text = report_of(&run, state);
    assert!(text.contains("needs a maintainer"), "{text}");
    assert!(text.contains("a security patch would be dropped"), "{text}");
    assert!(text.contains("turn 0: a | round 1: b"), "{text}");
}

#[test]
fn a_ladder_that_wrote_no_memo_still_shows_its_history_from_the_note() {
    // A patch-driven or budget-stopped ladder leaves no `ladder_reason`.
    let run = serde_json::json!({
        "event": "package_finished",
        "pkg": "foo",
        "version": "2.0",
        "outcome": {"Failed": {
            "reason": "foo could not be resolved: stopped on the budget.",
            "disk_full": false,
            "note": {"detail": "UNRESOLVED after the resolution ladder: foo: turn 0: a | round 1 not run"}
        }}
    });
    let state = r#"{"last_outcome":"failed","obs_name":"foo"}"#;
    let text = report_of(&run, state);
    assert!(!text.contains("Flaky tests and ladder"), "{text}");
    assert!(text.contains("turn 0: a | round 1 not run"), "{text}");
}

#[test]
fn a_needs_maintainer_event_carries_its_detail_to_the_outcome() {
    let mut p = Parsed::default();
    p.finished = Some(serde_json::json!({
        "event": "package_finished",
        "outcome": {"NeedsMaintainer": {"reason": "stop", "note": {"detail": "all of it"}}}
    }));
    let o = outcome(&p, None, Path::new("/work"), "foo", "foo", None);
    assert_eq!(o.result, "needs a maintainer");
    assert_eq!(o.detail.as_deref(), Some("stop"));
    assert_eq!(o.failure_detail.as_deref(), Some("all of it"));
}
