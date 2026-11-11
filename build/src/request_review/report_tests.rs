use super::gather::Skipped;
use super::render::{render_json, render_text};
use super::report::*;
use super::request::{ActionSource, ActionTarget};
use super::{Finding, Origin, Severity, ids};

fn finding(id: &'static str, severity: Severity) -> Finding {
    Finding::check(id, severity, "cracklib", "something to say")
}

fn agent_finding(severity: Severity) -> Finding {
    Finding {
        id: "agent.finding",
        severity,
        origin: Origin::Agent,
        package: "cracklib".into(),
        file: Some("cracklib.spec".into()),
        line: Some(73),
        message: "the claim".into(),
    }
}

fn report(findings: Vec<Finding>, agent: Option<AgentSection>) -> Report {
    let mut r = Report {
        schema: SCHEMA,
        auto_package: "test".into(),
        request: RequestSummary {
            id: "1394662".into(),
            state: "new".into(),
            creator: "jdoe".into(),
            created: "2026-11-03T11:14:44".into(),
            description: "d".into(),
            findings: Vec::new(),
        },
        actions: vec![ActionReport {
            source: ActionSource {
                project: "home:u:branches:Base:System".into(),
                package: "cracklib".into(),
                rev: Some("3".into()),
            },
            target: ActionTarget {
                project: "Base:System".into(),
                package: "cracklib".into(),
            },
            old_version: Some("2.10.3".into()),
            new_version: Some("2.10.4".into()),
            changed: vec!["cracklib.spec".into()],
            added: vec!["cracklib-2.10.4.tar.bz2".into()],
            removed: vec!["cracklib-2.10.3.tar.bz2".into()],
            findings,
            skipped: vec![Skipped {
                check: "signatures".into(),
                why: "no signature in the package".into(),
            }],
        }],
        agent,
        fail_on: Severity::Warning,
        fail_on_agent: false,
        exit: 0,
    };
    r.exit = exit_code(&r);
    r
}

fn section(findings: Vec<Finding>) -> AgentSection {
    AgentSection {
        ran: true,
        model: Some("claude-sonnet-5-5".into()),
        cost_usd: Some(0.42),
        error: None,
        note: AGENT_NOTE,
        findings,
    }
}

#[test]
fn the_exit_code_follows_the_threshold_over_the_checks_findings() {
    let mut r = report(vec![finding(ids::BUILD_PENDING, Severity::Warning)], None);
    for (fail_on, exit) in [
        (Severity::Blocker, 0),
        (Severity::Warning, 1),
        (Severity::Note, 1),
    ] {
        r.fail_on = fail_on;
        assert_eq!(exit_code(&r), exit, "{fail_on:?}");
    }
    let r = report(vec![finding(ids::SPEC_STYLE, Severity::Note)], None);
    assert_eq!(r.exit, 0);
}

#[test]
fn the_agents_findings_never_decide_the_exit_unless_asked() {
    let a = section(vec![agent_finding(Severity::Blocker)]);
    let mut r = report(Vec::new(), Some(a));
    assert_eq!(exit_code(&r), 0);
    r.fail_on_agent = true;
    assert_eq!(exit_code(&r), 1);
    r.fail_on = Severity::Blocker;
    r.agent.as_mut().unwrap().findings = vec![agent_finding(Severity::Warning)];
    assert_eq!(exit_code(&r), 0);
}

#[test]
fn the_text_has_its_sections_in_order() {
    let r = report(
        vec![
            finding(ids::SPEC_STYLE, Severity::Note),
            finding(ids::BUILD_PENDING, Severity::Warning),
        ],
        None,
    );
    let text = render_text(&r);
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in\n{text}"));
    assert!(at("Request 1394662 (new, by jdoe, created 2026-11-03 11:14)") < at("Blockers (0)"));
    assert!(at("Blockers (0)") < at("Warnings (1)"));
    assert!(at("Warnings (1)") < at("Notes (1)"));
    assert!(at("Notes (1)") < at("Not checked"));
    assert!(at("Not checked") < at("Result: 1 finding at or above warning -> exit 1"));
    assert!(text.contains("submit home:u:branches:Base:System/cracklib@3 -> Base:System/cracklib"));
    assert!(text.contains(
        "version 2.10.3 -> 2.10.4; changed: cracklib.spec; added: cracklib-2.10.4.tar.bz2"
    ));
    assert!(text.contains("  build.pending  something to say"));
    assert!(!text.contains("Agent review"));
}

#[test]
fn the_agent_section_is_titled_as_opinion_and_each_line_says_agent() {
    let r = report(
        Vec::new(),
        Some(section(vec![agent_finding(Severity::Note)])),
    );
    let text = render_text(&r);
    assert!(text.contains("Agent review (model opinion, not verified; claude-sonnet-5-5, $0.42)"));
    assert!(text.contains("  AGENT: note cracklib.spec:73: the claim"));
    assert!(text.contains(AGENT_NOTE));
    assert!(text.contains("Result: 0 findings at or above warning -> exit 0"));
    let mut failed = section(Vec::new());
    failed.ran = false;
    failed.error = Some("not logged in".into());
    let text = render_text(&report(Vec::new(), Some(failed)));
    assert!(text.contains("Agent review: not run: not logged in"));
}

#[test]
fn the_json_has_schema_one_and_keeps_the_agents_findings_apart() {
    let r = report(
        vec![finding(ids::BUILD_PENDING, Severity::Warning)],
        Some(section(vec![agent_finding(Severity::Note)])),
    );
    let v: serde_json::Value = serde_json::from_str(&render_json(&r)).unwrap();
    assert_eq!(v["schema"], 1);
    assert_eq!(v["exit"], 1);
    assert_eq!(v["fail_on"], "warning");
    assert_eq!(v["request"]["id"], "1394662");
    let f = &v["actions"][0]["findings"];
    assert_eq!(f.as_array().unwrap().len(), 1);
    assert_eq!(f[0]["origin"], "check");
    assert_eq!(v["agent"]["findings"][0]["origin"], "agent");
    assert_eq!(v["agent"]["model"], "claude-sonnet-5-5");
    assert!(
        v["agent"]["note"]
            .as_str()
            .unwrap()
            .contains("not ground truth")
    );
    assert_eq!(v["actions"][0]["source"]["rev"], "3");
    let none = report(Vec::new(), None);
    let v: serde_json::Value = serde_json::from_str(&render_json(&none)).unwrap();
    assert!(v["agent"].is_null());
}

#[test]
fn with_fail_on_agent_a_turn_that_did_not_run_is_exit_two() {
    let mut failed = section(Vec::new());
    failed.ran = false;
    failed.error = Some("budget reached".into());
    let mut r = report(Vec::new(), Some(failed));
    assert_eq!(exit_code(&r), 0, "without the flag the agent is optional");
    r.fail_on_agent = true;
    assert_eq!(exit_code(&r), 2);
    r.exit = exit_code(&r);
    assert_eq!(r.exit, 2);
    assert!(
        render_text(&r)
            .contains("the agent turn did not run and --fail-on-agent is given -> exit 2")
    );
    // A turn that ran and found nothing passes.
    let mut ok = report(Vec::new(), Some(section(Vec::new())));
    ok.fail_on_agent = true;
    assert_eq!(exit_code(&ok), 0);
    // A check's finding still decides the other cases.
    let mut warn = report(vec![finding(ids::BUILD_PENDING, Severity::Warning)], None);
    warn.fail_on_agent = true;
    assert_eq!(exit_code(&warn), 1);
}

#[test]
fn a_terminal_escape_in_any_text_never_reaches_the_text_or_the_json() {
    let attack = "x\x1b[2A\x1b[J\u{202e}y\u{200b}z\x07";
    let mut r = report(
        Vec::new(),
        Some(section(vec![agent_finding(Severity::Note)])),
    );
    r.request.creator = attack.into();
    r.request.description = format!("line one\n{attack}");
    r.actions[0].added = vec![attack.into()];
    r.actions[0].source.project = attack.into();
    r.actions[0].skipped[0].why = attack.into();
    r.agent.as_mut().unwrap().findings[0].message = attack.into();
    r.agent.as_mut().unwrap().model = Some(attack.into());
    let text = render_text(&r);
    let json = render_json(&r);
    for out in [&text, &json] {
        assert!(
            !out.chars()
                .any(|c| c == '\x1b' || c == '\x07' || c == '\u{202e}' || c == '\u{200b}'),
            "{out:?}"
        );
        assert!(out.contains("x[2A[Jyz"));
    }
    assert!(text.lines().all(|l| !l.contains('\x1b')));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        v["request"]["description"]
            .as_str()
            .unwrap()
            .starts_with("line one\n")
    );
}
