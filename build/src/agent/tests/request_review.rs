//! Tests for a RequestReview turn: its grants, its schema and its model.

use super::*;
use crate::agent::build_prompt::build_turn_prompt;

fn ws() -> &'static std::path::Path {
    std::path::Path::new("/srv/review-ws")
}

fn tools(hosts: &[&str]) -> Vec<String> {
    let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
    request_review_tools(ws(), &hosts)
        .split(',')
        .map(str::to_string)
        .collect()
}

#[test]
fn a_request_review_reads_and_searches_and_nothing_else() {
    let granted = turn_allowed_tools(Task::RequestReview, ws(), None);
    assert!(granted.contains("Read(//srv/review-ws/**)"), "{granted}");
    assert!(granted.split(',').any(|t| t == "Grep"), "{granted}");
    for forbidden in ["Edit", "Write", "Bash", "Web", "NotebookEdit", "Skill"] {
        assert!(!granted.contains(forbidden), "{forbidden} in {granted}");
    }
    // The grant is the same with a checkout, which a review does not have.
    assert_eq!(
        granted,
        turn_allowed_tools(Task::RequestReview, ws(), Some(std::path::Path::new("/x")))
    );
}

#[test]
fn only_the_named_hosts_can_be_fetched_and_only_when_plain() {
    assert_eq!(
        tools(&["github.com"]),
        [
            "Grep",
            "Read(//srv/review-ws/**)",
            "WebFetch(domain:github.com)"
        ]
    );
    // A name that could widen the grant is dropped, not escaped.
    for bad in [
        "*.github.com",
        "github.com,Bash(*)",
        "a.com)",
        "-x.com",
        ".com",
        "localhost",
        "GitHub.com",
        "a b.com",
    ] {
        assert_eq!(tools(&[bad]).len(), 2, "{bad}");
    }
    assert!(!tools(&[]).iter().any(|t| t.contains("Web")));
}

#[test]
fn the_schema_requires_findings_and_allows_only_no_change() {
    let schema: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::RequestReview)).unwrap();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(required.contains(&"findings"));
    assert_eq!(
        schema["properties"]["action"]["enum"],
        serde_json::json!(["no_change"])
    );
    let item = &schema["properties"]["findings"]["items"];
    assert_eq!(
        item["properties"]["severity"]["enum"],
        serde_json::json!(["blocker", "warning", "note"])
    );
    // Every other task keeps its own schema: no findings, every action.
    let other: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::Review)).unwrap();
    assert!(other["properties"].get("findings").is_none());
    assert!(
        !other["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "findings")
    );
    assert_eq!(
        other["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn a_request_review_never_runs_below_sonnet() {
    let sonnet = "claude-sonnet-5-5";
    assert_eq!(model_for_task(Task::RequestReview, 1, 0), sonnet);
    assert_eq!(model_for_task(Task::RequestReview, 1, 2), sonnet);
    for rotation in 0..6 {
        assert_ne!(
            model_for_task(Task::RequestReview, 1, rotation),
            "claude-haiku-5-5"
        );
    }
    assert_eq!(model_for_task(Task::Review, 1, 0), model_for_attempt(1, 0));
}

#[test]
fn a_request_review_may_not_edit_the_service() {
    assert!(!super::super::owned_files::may_edit_service(
        Task::RequestReview
    ));
}

#[test]
fn the_prompt_of_a_request_review_says_what_it_is() {
    let ws = tempfile::tempdir().unwrap();
    let mut req = super::prompt::request(ws.path(), &[]);
    req.task = Task::RequestReview;
    req.context = Some("Request 1");
    let prompt = build_turn_prompt(&req);
    assert!(prompt.contains("reviewing an OBS submit request"));
    assert!(prompt.contains("Request review. $2.00 of model budget left"));
    assert!(!prompt.contains("Editing the spec"));
    assert!(!prompt.contains("the spec file —"));
}
