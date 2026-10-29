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
