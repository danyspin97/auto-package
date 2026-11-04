//! Tests of the whole turn against a stub claude program.

use super::prompt::*;
use super::*;

// ── the real `turn` against a stub `claude` ──────────────────────────────
//
// The program the turn launches is replaced (per thread) by a shell script
// that prints canned stream-json, so the whole function runs: the argument
// list, the scrubbed environment, the process group and watchdog, the
// stdout/stderr readers and the response parsing. No sandbox (bwrap) is
// involved in `turn`: the isolation it applies is `env_clear` plus the
// allow-list, which the environment test below observes from the child.

struct Stub {
    dir: tempfile::TempDir,
}

impl Stub {
    /// A stub whose body is `script` (sh); the directory also holds the
    /// `env.txt` and `args.txt` it records and a `payload` file for `cat`.
    pub(super) fn new(script: &str, payload: &str) -> Stub {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("payload"), payload).unwrap();
        let body = format!(
            "#!/bin/sh\nd='{d}'\nenv > \"$d/env.txt\"\nprintf '%s\\n' \"$@\" > \"$d/args.txt\"\n{script}\n",
            d = dir.path().display()
        );
        let bin = dir.path().join("claude-stub");
        std::fs::write(&bin, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Stub { dir }
    }

    /// A stub that prints the `result` event carrying `response` as the
    /// structured output, then exits with `code`.
    fn answering(response: &serde_json::Value, code: i32) -> Stub {
        let event = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": response.to_string(),
            "total_cost_usd": 0.0123,
            "num_turns": 3,
            "session_id": "sess-1",
            "usage": {"input_tokens": 10, "output_tokens": 20}
        });
        Stub::new(
            &format!("cat \"$d/payload\"\nexit {code}"),
            &format!("{event}\n"),
        )
    }

    fn run(&self, overrides: impl FnOnce(&mut TurnTestOverrides)) -> Result<TurnOutcome, Error> {
        let ws = tempfile::tempdir().unwrap();
        TURN_TEST_OVERRIDES.with(|o| {
            let mut o = o.borrow_mut();
            *o = TurnTestOverrides {
                program: Some(self.dir.path().join("claude-stub")),
                ..Default::default()
            };
            overrides(&mut o);
        });
        let result = turn(request(ws.path(), &[]));
        TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
        result
    }

    fn recorded(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }
}

pub(super) fn answer(action: &str) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "why": if action == "blocked" { "the sdist ships no tests and upstream is gone" } else { "" },
        "reasoning": "r",
        "evidence": ["Read foo.spec"],
        "confidence": "high"
    })
}

#[test]
fn a_normal_edit_turn_is_parsed_end_to_end() {
    let stub = Stub::answering(&answer("edited"), 0);
    let out = stub.run(|_| {}).expect("turn");
    assert_eq!(out.action, Action::Edited);
    assert_eq!(out.evidence, vec!["Read foo.spec"]);
    assert_eq!(out.session_id.as_deref(), Some("sess-1"));
    assert_eq!(out.usage.cli_cost_usd, Some(0.0123));
    assert_eq!(out.usage.input_tokens, 10);
    assert!(out.ended_early.is_none());
    // The CLI was called the way the pipeline relies on.
    let args = stub.recorded("args.txt");
    for needle in [
        "--print",
        "stream-json",
        "--json-schema",
        "--permission-mode",
        "project",
        "--max-budget-usd",
    ] {
        assert!(
            args.lines().any(|l| l == needle),
            "{needle} missing from {args}"
        );
    }
    assert!(args.contains("--allowed-tools"), "{args}");
    assert!(args.lines().any(|l| l == "-p"), "{args}");
}

#[test]
fn a_blocked_answer_carries_its_reason() {
    let stub = Stub::answering(&answer("blocked"), 0);
    match stub.run(|_| {}).expect("turn").action {
        Action::Blocked { why } => assert!(why.contains("upstream is gone"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stage_files_and_patch_dependencies_reach_the_outcome() {
    let mut resp = answer("edited");
    resp["stage_files"] =
        serde_json::json!([{"from": "source/package-lock.json", "to": "package-lock.json"}]);
    resp["patch_dependencies"] = serde_json::json!([{"name": "torch", "problem": "ships utils in -devel",
            "condition": {"path": "torch/utils/benchmark/utils"}}]);
    let out = Stub::answering(&resp, 0).run(|_| {}).expect("turn");
    assert_eq!(out.stage_files.len(), 1);
    assert_eq!(out.stage_files[0].from, "source/package-lock.json");
    assert_eq!(out.stage_files[0].to, "package-lock.json");
    assert_eq!(out.patch_dependencies.len(), 1);
    assert_eq!(out.patch_dependencies[0].name, "torch");
    assert_eq!(
        out.patch_dependencies[0]
            .condition
            .as_ref()
            .map(|c| c.path.as_str()),
        Some("torch/utils/benchmark/utils")
    );
}

#[test]
fn malformed_json_in_the_result_is_a_parse_error() {
    let event = serde_json::json!({"type": "result", "subtype": "success", "result": "{\"action\": \"edited\", oops"});
    let stub = Stub::new("cat \"$d/payload\"", &format!("{event}\nnot json at all\n"));
    match stub.run(|_| {}) {
        Err(Error::Parse {
            message, stdout, ..
        }) => {
            assert!(message.contains("no usable turn response"), "{message}");
            assert!(stdout.contains("not json at all"), "{stdout}");
        }
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("{:?}", o.action),
    }
    // An action the pipeline does not know is a parse error too.
    match Stub::answering(&answer("rm_rf"), 0).run(|_| {}) {
        Err(Error::Parse { message, .. }) => assert!(message.contains("rm_rf"), "{message}"),
        _ => panic!("expected a parse error"),
    }
}

#[test]
fn a_nonzero_exit_without_a_response_is_a_failure_with_its_stderr() {
    let stub = Stub::new("echo 'segfault in the CLI' >&2\nexit 3", "");
    match stub.run(|_| {}) {
        Err(Error::Failed { code, stderr, .. }) => {
            assert_eq!(code, 3);
            assert!(stderr.contains("segfault in the CLI"), "{stderr}");
        }
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("{:?}", o.action),
    }
    // A usable response is still used when the CLI exits 1 after it.
    let out = Stub::answering(&answer("no_change"), 1)
        .run(|_| {})
        .expect("rescued");
    assert_eq!(out.action, Action::NoChange);
}

#[test]
fn a_structured_end_early_result_is_reported_as_such() {
    let event = serde_json::json!({"type": "result", "subtype": "error_max_budget_usd", "is_error": true,
            "errors": ["Reached maximum budget ($0.03)"], "result": ""});
    let stub = Stub::new("cat \"$d/payload\"\nexit 1", &format!("{event}\n"));
    match stub.run(|_| {}) {
        Err(Error::EndedEarly(e)) => assert_eq!(e.subtype, "error_max_budget_usd"),
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("{:?}", o.action),
    }
}

#[test]
fn a_turn_that_goes_quiet_is_killed_by_the_watchdog() {
    let stub = Stub::new("sleep 30", "");
    let started = std::time::Instant::now();
    let result = stub.run(|o| {
        o.idle_timeout = Some(std::time::Duration::from_millis(300));
        o.tick = Some(std::time::Duration::from_millis(50));
    });
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the stub was not killed"
    );
    match result {
        Err(Error::Failed { stderr, .. }) => {
            assert!(stderr.contains("claude turn was killed"), "{stderr}")
        }
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("{:?}", o.action),
    }
}

#[test]
fn a_turn_over_the_wall_clock_limit_is_killed_even_while_it_prints() {
    let stub = Stub::new(
        "while true; do echo '{\"type\":\"system\"}'; sleep 0.05; done",
        "",
    );
    let result = stub.run(|o| {
        o.turn_timeout = Some(std::time::Duration::from_millis(400));
        o.tick = Some(std::time::Duration::from_millis(50));
    });
    match result {
        Err(Error::Failed { stderr, .. }) => {
            assert!(stderr.contains("claude turn was killed"), "{stderr}")
        }
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("{:?}", o.action),
    }
}

#[test]
fn a_missing_program_is_a_launch_error() {
    let ws = tempfile::tempdir().unwrap();
    TURN_TEST_OVERRIDES.with(|o| {
        *o.borrow_mut() = TurnTestOverrides {
            program: Some(ws.path().join("no-such-claude")),
            ..Default::default()
        }
    });
    let result = turn(request(ws.path(), &[]));
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    assert!(
        matches!(result, Err(Error::Launch(_))),
        "{:?}",
        result.err().map(|e| e.to_string())
    );
}

#[test]
fn the_child_gets_a_scrubbed_environment() {
    // SAFETY: these names are used by no other test.
    unsafe {
        std::env::set_var("AP_STUBTEST_SERVICE_TOKEN", "leak-me-1");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "leak-me-2");
        std::env::set_var("OSC_PASSWORD", "leak-me-3");
        std::env::set_var("AP_STUBTEST_PLAIN", "leak-me-4");
    }
    let stub = Stub::answering(&answer("no_change"), 0);
    stub.run(|_| {}).expect("turn");
    let env = stub.recorded("env.txt");
    for secret in ["leak-me-1", "leak-me-2", "leak-me-3", "leak-me-4"] {
        assert!(!env.contains(secret), "{secret} reached the child:\n{env}");
    }
    assert!(env.lines().any(|l| l.starts_with("PATH=")), "{env}");
    assert!(env.lines().any(|l| l == "QUILTRC=/dev/null"), "{env}");
}

#[test]
fn a_turn_that_rewrites_the_checkouts_apiurl_is_rejected_and_the_file_put_back() {
    // The whole turn, with a stub that writes the checkout's `_apiurl` and then
    // answers: the turn is rejected and the file is back as it was.
    let co = tempfile::tempdir().unwrap();
    let apiurl = co.path().join(".osc/_apiurl");
    std::fs::create_dir(co.path().join(".osc")).unwrap();
    std::fs::write(&apiurl, "https://api.example\n").unwrap();
    let event = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": answer("edited").to_string(),
        "total_cost_usd": 0.01,
        "num_turns": 1,
        "session_id": "sess-2",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let stub = Stub::new(
        &format!(
            "printf 'http://evil\\n' > '{}'\ncat \"$d/payload\"",
            apiurl.display()
        ),
        &format!("{event}\n"),
    );
    let ws = tempfile::tempdir().unwrap();
    let mut req = request(ws.path(), &[]);
    req.checkout_dir = Some(co.path());
    TURN_TEST_OVERRIDES.with(|o| {
        o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
    });
    let result = turn(req);
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    let Err(e) = result else {
        panic!("a turn that rewrote _apiurl was accepted");
    };
    assert!(matches!(e, Error::Tampered { .. }), "{e}");
    assert!(e.to_string().contains("modified osc/git metadata"), "{e}");
    assert!(e.usage().is_some(), "the turn's cost is still charged");
    assert_eq!(std::fs::read(&apiurl).unwrap(), b"https://api.example\n");
}

/// A stub whose turn runs `script` and then answers `response`.
fn answering_after(script: &str, response: &serde_json::Value) -> Stub {
    let event = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": response.to_string(),
        "total_cost_usd": 0.01,
        "num_turns": 1,
        "session_id": "sess-3",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    Stub::new(
        &format!("{script}\ncat \"$d/payload\""),
        &format!("{event}\n"),
    )
}

/// A turn of `task` on a checkout whose `_service` is a well-formed one, where the
/// stub rewrites that `_service`. Returns what the turn returned, the `_service`
/// afterwards and the arguments the CLI was given.
fn service_turn(task: Task) -> (Result<TurnOutcome, Error>, Vec<u8>, String) {
    let co = tempfile::tempdir().unwrap();
    let service = co.path().join("_service");
    std::fs::write(&service, "<services/>\n").unwrap();
    let script = format!(
        "printf '<services><service name=\"x\"/></services>\\n' > '{}'",
        service.display()
    );
    let stub = answering_after(&script, &answer("edited"));
    let ws = tempfile::tempdir().unwrap();
    let mut req = request(ws.path(), &[]);
    req.task = task;
    req.checkout_dir = Some(co.path());
    TURN_TEST_OVERRIDES.with(|o| {
        o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
    });
    let result = turn(req);
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    let args = stub.recorded("args.txt");
    (result, std::fs::read(&service).unwrap(), args)
}

#[test]
fn a_turn_that_may_edit_the_service_keeps_its_change_to_it() {
    for task in [Task::Review, Task::FixAfterBuild, Task::PatchDependency] {
        let (result, service, args) = service_turn(task);
        assert!(result.is_ok(), "{task:?}: {:?}", result.err());
        assert_eq!(
            service, b"<services><service name=\"x\"/></services>\n",
            "{task:?}"
        );
        assert!(
            !args.contains("_service)"),
            "{task:?} is not denied it: {args}"
        );
    }
}

#[test]
fn a_turn_that_may_not_edit_the_service_is_denied_it_and_its_change_is_tampered() {
    for task in [
        Task::RefreshPatches,
        Task::PatchReview,
        Task::FixBug,
        Task::TranslateUnresolvableDeps,
    ] {
        let (result, service, args) = service_turn(task);
        let Err(e) = result else {
            panic!("{task:?}: a change to the _service was accepted");
        };
        assert!(matches!(e, Error::Tampered { .. }), "{task:?}: {e}");
        assert!(e.to_string().contains("_service"), "{task:?}: {e}");
        assert!(
            e.usage().is_some(),
            "{task:?}: the turn's cost is still charged"
        );
        assert_eq!(service, b"<services/>\n", "{task:?}: put back");
        assert!(
            args.contains("_service)"),
            "{task:?}: denied in the CLI: {args}"
        );
    }
}

#[test]
fn a_patch_review_turn_reads_the_rebase_series_like_a_refresh_turn() {
    for (task, want) in [
        (Task::PatchReview, true),
        (Task::RefreshPatches, true),
        (Task::FixBug, true),
        (Task::Review, false),
    ] {
        let stub = Stub::answering(&answer("no_change"), 0);
        let ws = tempfile::tempdir().unwrap();
        let mut req = request(ws.path(), &[]);
        req.task = task;
        TURN_TEST_OVERRIDES.with(|o| {
            o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
        });
        let _ = turn(req);
        TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
        let env = stub.recorded("env.txt");
        let has = env.lines().any(|l| l == "QUILT_SERIES=series-rebase");
        assert_eq!(has, want, "{task:?}:\n{env}");
    }
}

/// A PatchReview turn against a stub that gives `response`.
fn review_turn(response: &serde_json::Value) -> Result<TurnOutcome, Error> {
    let stub = Stub::answering(response, 0);
    let ws = tempfile::tempdir().unwrap();
    let mut req = request(ws.path(), &[]);
    req.task = Task::PatchReview;
    TURN_TEST_OVERRIDES.with(|o| {
        o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
    });
    let result = turn(req);
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    result
}

#[test]
fn a_patch_review_verdict_reaches_the_outcome() {
    let mut response = answer("no_change");
    response["review"] = serde_json::json!({"verdict": "accept", "reason": "same intent"});
    let out = review_turn(&response).expect("turn");
    let review = out.patch_review.expect("a verdict");
    assert_eq!(review.verdict, PatchReviewVerdict::Accept);
    assert_eq!(review.reason, "same intent");
}

#[test]
fn a_patch_review_answer_without_a_verdict_is_no_usable_response() {
    let Err(Error::Parse { message, .. }) = review_turn(&answer("no_change")) else {
        panic!("an answer without `review` was accepted");
    };
    assert!(message.contains("no usable turn response"), "{message}");
    assert!(message.contains("`review`"), "{message}");
    // Giving up needs no verdict.
    let out = review_turn(&answer("blocked")).expect("a blocked review is an answer");
    assert!(out.patch_review.is_none());
}

/// A FixBug turn against a stub that gives `response`.
fn fix_turn(response: &serde_json::Value) -> Result<TurnOutcome, Error> {
    let stub = Stub::answering(response, 0);
    let ws = tempfile::tempdir().unwrap();
    let mut req = request(ws.path(), &[]);
    req.task = Task::FixBug;
    TURN_TEST_OVERRIDES.with(|o| {
        o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
    });
    let result = turn(req);
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    result
}

#[test]
fn a_fixed_verdict_reaches_the_outcome() {
    let mut response = answer("edited");
    response["fix"] = serde_json::json!({
        "verdict": "fixed",
        "patch": "foo-CVE-2025-1234.patch",
        "upstream": "https://github.com/o/r/commit/abc1234",
        "reason": "adds the length check"
    });
    let out = fix_turn(&response).expect("turn");
    let fix = out.bug_fix.expect("a verdict");
    assert_eq!(fix.verdict, FixVerdict::Fixed);
    assert_eq!(fix.patch, "foo-CVE-2025-1234.patch");
    assert!(fix.evidence.is_empty());
}

#[test]
fn a_bug_fix_answer_without_a_verdict_is_no_usable_response() {
    let Err(Error::Parse { message, .. }) = fix_turn(&answer("edited")) else {
        panic!("an answer without `fix` was accepted");
    };
    assert!(message.contains("no usable turn response"), "{message}");
    assert!(message.contains("`fix`"), "{message}");
    // Giving up needs no verdict.
    let out = fix_turn(&answer("blocked")).expect("a blocked fix is an answer");
    assert!(out.bug_fix.is_none());
}

#[test]
fn a_bug_fix_turn_gets_the_hosts_it_was_given_when_trusted_and_no_search() {
    let stub = Stub::answering(&answer("no_change"), 0);
    let ws = tempfile::tempdir().unwrap();
    let hosts = ["codeberg.org".to_string(), "evil.example.org".to_string()];
    let mut req = request(ws.path(), &[]);
    req.task = Task::FixBug;
    req.fetch_hosts = &hosts;
    TURN_TEST_OVERRIDES.with(|o| {
        o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
    });
    let _ = turn(req);
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    let args = stub.recorded("args.txt");
    assert!(args.contains("WebFetch(domain:codeberg.org)"), "{args}");
    assert!(!args.contains("evil.example.org"), "{args}");
    assert!(!args.contains("WebSearch"), "{args}");
}

/// A RequestReview turn against a stub that gives `response`.
fn request_review_turn(response: &serde_json::Value) -> Result<TurnOutcome, Error> {
    let stub = Stub::answering(response, 0);
    let ws = tempfile::tempdir().unwrap();
    let mut req = request(ws.path(), &[]);
    req.task = Task::RequestReview;
    TURN_TEST_OVERRIDES.with(|o| {
        o.borrow_mut().program = Some(stub.dir.path().join("claude-stub"));
    });
    let result = turn_with_hosts(req, &["github.com".to_string()]);
    TURN_TEST_OVERRIDES.with(|o| *o.borrow_mut() = TurnTestOverrides::default());
    let args = stub.recorded("args.txt");
    let lines: Vec<&str> = args.lines().collect();
    let at = lines.iter().position(|l| *l == "--allowed-tools").unwrap();
    let granted = lines[at + 1];
    assert!(granted.contains("WebFetch(domain:github.com)"), "{granted}");
    assert!(
        !granted.contains("Edit(") && !granted.contains("Bash("),
        "{granted}"
    );
    result
}

#[test]
fn the_findings_of_a_request_review_reach_the_outcome() {
    let mut response = answer("no_change");
    response["findings"] = serde_json::json!([
        {"severity": "warning", "file": "new/foo.spec", "line": 7,
         "claim": "the bump needs no new BuildRequires", "evidence": "BuildRequires: x"},
        {"severity": "note", "file": "request.xml", "claim": "c", "evidence": "e"}
    ]);
    let out = request_review_turn(&response).expect("turn");
    assert_eq!(out.review_findings.len(), 2);
    assert_eq!(out.review_findings[0].line, Some(7));
    assert_eq!(out.review_findings[1].line, None);
    assert_eq!(out.review_findings[1].severity, "note");
}

#[test]
fn a_request_review_answer_without_findings_is_no_usable_response() {
    let Err(Error::Parse { message, .. }) = request_review_turn(&answer("no_change")) else {
        panic!("an answer without `findings` was accepted");
    };
    assert!(message.contains("`findings`"), "{message}");
    // Nothing found is an answer.
    let mut none = answer("no_change");
    none["findings"] = serde_json::json!([]);
    assert!(
        request_review_turn(&none)
            .expect("turn")
            .review_findings
            .is_empty()
    );
}
