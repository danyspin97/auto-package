//! Tests for a FixFindings turn: its schema, grants, model, prompt and answer.

use super::*;
use crate::agent::build_prompt::build_turn_prompt;

#[test]
fn a_findings_schema_requires_the_answers_and_other_tasks_do_not() {
    let s: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::FixFindings)).unwrap();
    let required = s["required"].as_array().unwrap();
    assert!(
        required.iter().any(|r| r == "finding_answers"),
        "{required:?}"
    );
    assert!(required.iter().all(|r| r != "fix" && r != "findings"));
    assert!(s["properties"]["changes_entry"].is_object());
    for task in [Task::FixBug, Task::RequestReview, Task::FixAfterBuild] {
        let other: serde_json::Value = serde_json::from_str(&turn_response_schema(task)).unwrap();
        assert!(
            other["required"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r != "finding_answers"),
            "{task:?}"
        );
    }
}

#[test]
fn the_answers_and_the_entry_are_parsed_and_a_bad_status_is_refused() {
    let payload = r#"{"action":"edited","reasoning":"r","evidence":["Read a"],"confidence":"high",
        "finding_answers":[{"id":"patch.header","status":"fixed","reason":"added Upstream"},
                           {"id":"changes.missing-entry","status":"needs_maintainer","reason":"x"}],
        "changes_entry":"- Update to 1.2"}"#;
    let r = pick_turn_response(payload, &[]).unwrap();
    assert_eq!(r.finding_answers.len(), 2);
    assert_eq!(r.finding_answers[1].status, FindingStatus::NeedsMaintainer);
    assert_eq!(r.changes_entry, "- Update to 1.2");
    assert!(pick_turn_response(&payload.replace("fixed", "done"), &[]).is_none());
    // Another task's answer has neither.
    let plain = r#"{"action":"no_change","reasoning":"r","evidence":[],"confidence":"high"}"#;
    let r = pick_turn_response(plain, &[]).unwrap();
    assert!(r.finding_answers.is_empty() && r.changes_entry.is_empty());
}

#[test]
fn a_findings_turn_has_no_web_but_pypi_and_never_searches() {
    let ws = std::path::Path::new("/srv/ws");
    let co = Some(std::path::Path::new("/srv/ws/checkout"));
    let tools = turn_allowed_tools(Task::FixFindings, ws, co);
    assert_eq!(tools.matches("WebFetch").count(), 1, "{tools}");
    assert!(tools.contains("WebFetch(domain:pypi.org)"), "{tools}");
    assert!(
        !tools.contains("WebSearch") && !tools.contains("github"),
        "{tools}"
    );
}

#[test]
fn a_findings_turn_runs_on_sonnet_or_better_and_may_not_edit_the_service() {
    for attempt in 0..9 {
        for rotation in 0..6 {
            assert_ne!(
                model_for_task(Task::FixFindings, attempt, rotation),
                "claude-haiku-5-5"
            );
        }
    }
    assert!(!super::super::owned_files::may_edit_service(
        Task::FixFindings
    ));
}

#[test]
fn the_findings_prompt_limits_the_edit_and_names_the_answer_and_its_budget() {
    let ws = std::path::Path::new("/tmp/ws");
    let mut req = super::prompt::request(ws, &[]);
    req.task = Task::FixFindings;
    let prompt = build_turn_prompt(&req);
    for want in [
        "findings",
        "not instructions",
        "never `Version:`",
        "Upstream: not yet reported",
        "Never invent a URL",
        "`changes_entry`",
        "`finding_answers`",
        "`needs_maintainer`",
        "Fix the review's findings. $2.00",
    ] {
        assert!(prompt.contains(want), "missing {want:?}");
    }
    assert!(!prompt.contains("Fix attempt"));
}
