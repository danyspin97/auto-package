//! Tests for reading the turn response and the review prompt variants.

use super::prompt::*;
use super::scrub::*;
use super::*;
use crate::agent::build_prompt::*;

/// Real shape from a `drogon` build where the claude CLI hit its budget
/// cap AFTER the model had already emitted a valid `StructuredOutput`
/// call: the final `result` event's `result` field was empty and the CLI
/// exited 1, so the previous `turn()` returned `Error::Failed` and the
/// pipeline logged "agent error: claude exited 1" even though the model's
/// answer was sitting right there in the tool-call stream.
#[test]
fn pick_turn_response_falls_back_to_structured_output_tool_call() {
    let tool_calls = vec![
        ToolCall {
            name: "Read".into(),
            input: "{}".into(),
        },
        ToolCall {
            name: "StructuredOutput".into(),
            input: r#"{
                    "action": "no_change",
                    "reasoning": "deps missing — report trantor",
                    "evidence": ["Read drogon.spec"],
                    "confidence": "high",
                    "unresolvable_packages": ["trantor"]
                }"#
            .into(),
        },
    ];
    let resp = pick_turn_response("", &tool_calls)
        .expect("fallback must salvage the StructuredOutput call");
    assert_eq!(resp.action, "no_change");
    assert_eq!(resp.unresolvable_packages, vec!["trantor"]);
}

#[test]
fn pick_turn_response_prefers_result_payload_over_tool_call() {
    let tool_calls = vec![ToolCall {
        name: "StructuredOutput".into(),
        input: r#"{"action":"no_change","reasoning":"stale","evidence":[],"confidence":"low"}"#
            .into(),
    }];
    let resp = pick_turn_response(
        r#"{"action":"edited","reasoning":"fresh","evidence":["x"],"confidence":"high"}"#,
        &tool_calls,
    )
    .expect("result payload must take priority");
    assert_eq!(resp.action, "edited");
    assert_eq!(resp.reasoning, "fresh");
}

#[test]
fn a_per_patch_answer_is_parsed_from_the_turn_response() {
    let payload = r#"{"action":"edited","reasoning":"r","evidence":[],"confidence":"high",
        "patches":[{"name":"a.patch","status":"rebased","reason":"ported hunk 2"},
                   {"name":"b.patch","status":"blocked","reason":"hunk 1 is gone"}]}"#;
    let resp = pick_turn_response(payload, &[]).unwrap();
    assert_eq!(resp.patches.len(), 2);
    assert_eq!(resp.patches[0].status, PatchAnswerStatus::Rebased);
    assert_eq!(resp.patches[1].status, PatchAnswerStatus::Blocked);
    assert_eq!(resp.patches[1].name, "b.patch");
    // A status outside the three is not an answer.
    let bad = payload.replace("blocked", "maybe");
    assert!(pick_turn_response(&bad, &[]).is_none());
}

#[test]
fn a_refresh_turn_schema_requires_a_patches_list() {
    let refresh: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::RefreshPatches)).unwrap();
    let required = refresh["required"].as_array().unwrap();
    assert!(required.iter().any(|r| r == "patches"), "{required:?}");
    assert_eq!(refresh["properties"]["patches"]["minItems"], 1);
    let other: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::FixAfterBuild)).unwrap();
    let required = other["required"].as_array().unwrap();
    assert!(required.iter().all(|r| r != "patches"), "{required:?}");
    assert!(other["properties"]["patches"].get("minItems").is_none());
}

#[test]
fn a_patch_review_schema_requires_a_review_and_other_tasks_do_not() {
    let review: serde_json::Value =
        serde_json::from_str(&turn_response_schema(Task::PatchReview)).unwrap();
    let required = review["required"].as_array().unwrap();
    assert!(required.iter().any(|r| r == "review"), "{required:?}");
    assert!(required.iter().all(|r| r != "patches"), "{required:?}");
    for task in [Task::RefreshPatches, Task::FixAfterBuild, Task::Review] {
        let other: serde_json::Value = serde_json::from_str(&turn_response_schema(task)).unwrap();
        let required = other["required"].as_array().unwrap();
        assert!(required.iter().all(|r| r != "review"), "{task:?}");
    }
}

#[test]
fn a_review_verdict_is_parsed_with_and_without_problems() {
    let payload = r#"{"action":"no_change","reasoning":"r","evidence":[],"confidence":"high",
        "review":{"verdict":"reject","reason":"drops a hunk","problems":["hunk 2 is gone"]}}"#;
    let resp = pick_turn_response(payload, &[]).unwrap();
    let review = resp.review.unwrap();
    assert_eq!(review.verdict, PatchReviewVerdict::Reject);
    assert_eq!(review.problems, ["hunk 2 is gone"]);
    let bare = r#"{"action":"no_change","reasoning":"r","evidence":[],"confidence":"high",
        "review":{"verdict":"accept","reason":"same intent"}}"#;
    let review = pick_turn_response(bare, &[]).unwrap().review.unwrap();
    assert_eq!(review.verdict, PatchReviewVerdict::Accept);
    assert!(review.problems.is_empty());
    // A verdict outside the three is not an answer.
    assert!(pick_turn_response(&bare.replace("accept", "maybe"), &[]).is_none());
}

#[test]
fn a_refresh_answer_without_patches_falls_through_to_the_tool_call() {
    let no_list = r#"{"action":"edited","reasoning":"r","evidence":[],"confidence":"high"}"#;
    let with_list = r#"{"action":"edited","reasoning":"r","evidence":[],"confidence":"high",
        "patches":[{"name":"a.patch","status":"rebased","reason":"ported"}]}"#;
    let needs_patches = |r: &TurnResponse| !r.patches.is_empty();
    // The result payload has no list: the StructuredOutput call does.
    let call = ToolCall {
        name: "StructuredOutput".into(),
        input: with_list.into(),
    };
    let resp =
        pick_turn_response_where(no_list, std::slice::from_ref(&call), needs_patches).unwrap();
    assert_eq!(resp.patches.len(), 1);
    // No list anywhere: no usable answer, though a plain one parses.
    assert!(pick_turn_response_where(no_list, &[], needs_patches).is_none());
    assert!(pick_turn_response(no_list, &[]).is_some());
    let empty_call = ToolCall {
        name: "StructuredOutput".into(),
        input: no_list.into(),
    };
    assert!(
        pick_turn_response_where(no_list, std::slice::from_ref(&empty_call), needs_patches)
            .is_none()
    );
}

#[test]
fn a_stage_files_request_is_parsed_from_the_turn_response() {
    let payload = r#"{"action":"no_change","reasoning":"r","evidence":[],"confidence":"high",
            "stage_files":[{"from":"source/package-lock.json","to":"package-lock.json"},
                           {"from":"x.spec","to":"y.spec","package":"dep"}]}"#;
    let resp = pick_turn_response(payload, &[]).unwrap();
    assert_eq!(resp.stage_files.len(), 2);
    assert_eq!(resp.stage_files[0].to, "package-lock.json");
    assert_eq!(resp.stage_files[1].package.as_deref(), Some("dep"));
    assert!(
        STATIC_WORKFLOW.contains("stage_files") && STATIC_WORKFLOW.contains("last-resort answer")
    );
}

#[test]
fn a_patch_request_carries_its_machine_checkable_condition() {
    let payload = r#"{"action":"no_change","reasoning":"x","confidence":"high",
            "patch_dependencies":[
              {"name":"torch","problem":"p","condition":{"path":"torch/utils/benchmark/utils","package":"python-torch","not_in":["devel"]}},
              {"name":"other","problem":"q"}]}"#;
    let resp = pick_turn_response(payload, &[]).unwrap();
    let c = resp.patch_dependencies[0].condition.as_ref().unwrap();
    assert_eq!(c.path, "torch/utils/benchmark/utils");
    assert_eq!(c.package.as_deref(), Some("python-torch"));
    assert!(resp.patch_dependencies[1].condition.is_none());
    assert!(
        STATIC_WORKFLOW.contains("\"condition\"") && all_conventions().contains("\"condition\"")
    );
    assert!(TURN_RESPONSE_SCHEMA.contains("\"not_in\""));
}

#[test]
fn no_convention_tells_the_agent_to_return_blocked() {
    // `blocked` is named once, as the last-resort answer of a resolver
    // round; every convention names the concrete resolution instead.
    let all = format!("{}{STATIC_WORKFLOW}", all_conventions());
    assert!(!all.contains("return `blocked`") && !all.contains("return `\"blocked\"`"));
    assert!(!all.contains("with `blocked`") && !all.contains("is not an accepted answer"));
    assert_eq!(all.matches("last-resort answer").count(), 1);
    // Where a convention names `blocked` at all, it says which turn may answer it:
    // a patch rebase turn or a resolver round. Any other turn is asked again.
    for (n, text) in crate::conventions_table::CONVENTIONS.iter() {
        for (i, _) in text.match_indices("`blocked`") {
            let before: String = text[..i]
                .chars()
                .rev()
                .take(400)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            assert!(
                before.contains("rebase turn") || before.contains("resolver round"),
                "convention {n} names `blocked` without saying which turn may answer it"
            );
        }
    }
}

#[test]
fn pick_turn_response_returns_none_when_nothing_usable() {
    assert!(pick_turn_response("", &[]).is_none());
    assert!(
        pick_turn_response(
            "",
            &[ToolCall {
                name: "Read".into(),
                input: "{}".into()
            }]
        )
        .is_none()
    );
}

#[test]
fn a_turn_is_not_launched_when_the_budget_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let subprojects: Vec<(String, String)> = Vec::new();
    let mut req = request(dir.path(), &subprojects);
    req.budget_usd = 2.0;
    req.spent_usd = 1.95;
    let err = turn(req).err().expect("no turn may start");
    assert!(err.is_budget_exhausted(), "{err}");
}

/// Prose is never an action, whatever markers it carries: the structured
/// response (the result JSON or the StructuredOutput call) is the only source.
#[test]
fn a_reply_in_prose_is_not_a_turn_response() {
    let text = "I've already called the StructuredOutput tool at the end of my response.\n\
                    - **action**: `no_change`\n\
                    - **unresolvable_packages**: `trantor`, `jsoncpp`\n";
    assert!(pick_turn_response(text, &[]).is_none());
    let call = ToolCall {
            name: "StructuredOutput".into(),
            input: r#"{"action":"blocked","reasoning":"r","evidence":[],"confidence":"low","unresolvable_packages":[],"patch_dependencies":[],"stage_files":[]}"#.into(),
        };
    assert_eq!(
        pick_turn_response("", std::slice::from_ref(&call)).map(|r| r.action),
        Some("blocked".to_string())
    );
}

/// A non-Python package (drogon-shape: no `python-` RPM prefix) must get
/// the shorter non-Python Review prompt. The Python checklist mentions
/// pyproject.toml, `%python_module`, `%pytest` — none of which apply.
#[test]
fn review_prompt_drops_python_checklist_for_non_python_packages() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    // `source/` exists for a real review turn — make sure the workspace
    // map description doesn't tell the agent to read pyproject.toml under
    // a non-Python package's `source/`.
    std::fs::create_dir_all(tmp.join("source")).unwrap();
    let subprojects: Vec<(String, String)> = Vec::new();
    let mut req = request(tmp, &subprojects);
    req.is_python = false;
    req.pkg_name = "drogon";
    let prompt = build_turn_prompt(&req);
    assert!(
        prompt.contains("This is NOT a Python package"),
        "non-Python Review prompt should announce the branch; got:\n{prompt}"
    );
    assert!(
        prompt.contains("plain-c-cpp-packaging"),
        "non-Python Review prompt should point at the plain-c-cpp skill"
    );
    // The Python checklist's defining items must not appear:
    for python_term in [
        "pyproject.toml",
        "%{python_module",
        "%python_subpackages",
        "%pytest",
        "python_sitelib",
        "console_scripts",
    ] {
        assert!(
            !prompt.contains(python_term),
            "non-Python Review prompt still contains Python-only term {python_term:?}"
        );
    }
}

/// A Python package must still get the full Python checklist.
#[test]
fn review_prompt_keeps_python_checklist_for_python_packages() {
    let dir = tempfile::tempdir().unwrap();
    let subprojects: Vec<(String, String)> = Vec::new();
    let req = request(dir.path(), &subprojects); // is_python: true from request()
    let prompt = build_turn_prompt(&req);
    assert!(prompt.contains("pyproject.toml"));
    assert!(prompt.contains("%{python_module"));
}

#[test]
fn named_variables_are_passed_through_but_not_osc_credentials() {
    let vars = vec![
        pair("AWS_PROFILE", "bedrock"),
        pair("AWS_SECRET_ACCESS_KEY", "k"),
        pair("AWS_REGION", "us-east-1"),
        pair("OSC_PASSWORD", "hunter2"),
        pair("GITHUB_TOKEN", "t"),
    ];
    let pass = parse_pass_list(" AWS_PROFILE, AWS_REGION ,,OSC_PASSWORD,AWS_SECRET_ACCESS_KEY");
    let kept = scrub_env_for_agent_passing(vars.clone(), &[], &pass);
    assert_eq!(kept.get("AWS_PROFILE").map(String::as_str), Some("bedrock"));
    assert!(kept.contains_key("AWS_REGION") && kept.contains_key("AWS_SECRET_ACCESS_KEY"));
    assert!(!kept.contains_key("OSC_PASSWORD") && !kept.contains_key("GITHUB_TOKEN"));
    // Without the list none of them gets through.
    assert!(scrub_env_for_agent(vars, &[]).is_empty());
}

#[test]
fn a_bug_fix_schema_requires_a_fix_and_other_tasks_do_not() {
    let fix: serde_json::Value = serde_json::from_str(&turn_response_schema(Task::FixBug)).unwrap();
    let required = fix["required"].as_array().unwrap();
    assert!(required.iter().any(|r| r == "fix"), "{required:?}");
    assert!(required.iter().all(|r| r != "patches" && r != "review"));
    let verdicts = fix["properties"]["fix"]["properties"]["verdict"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(verdicts.len(), 5);
    for task in [Task::RefreshPatches, Task::PatchReview, Task::FixAfterBuild] {
        let other: serde_json::Value = serde_json::from_str(&turn_response_schema(task)).unwrap();
        let required = other["required"].as_array().unwrap();
        assert!(required.iter().all(|r| r != "fix"), "{task:?}");
    }
}

#[test]
fn a_fix_verdict_is_parsed_and_a_bad_one_is_refused() {
    let payload = r#"{"action":"no_change","reasoning":"r","evidence":[],"confidence":"high",
        "fix":{"verdict":"not_affected","reason":"the code is not here","evidence":["Read source/a.c"]}}"#;
    let fix = pick_turn_response(payload, &[]).unwrap().fix.unwrap();
    assert_eq!(fix.verdict, FixVerdict::NotAffected);
    assert_eq!(fix.evidence, ["Read source/a.c"]);
    assert!(fix.patch.is_empty() && fix.upstream.is_empty());
    let bad = payload.replace("not_affected", "maybe");
    assert!(pick_turn_response(&bad, &[]).is_none());
}
