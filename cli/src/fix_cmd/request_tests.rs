use ap_build::bugfix::{FindingRoute, ReviewFinding};
use ap_build::osc;
use ap_build::request_review::Origin;
use ap_build::request_review::Severity;
use ap_build::request_review::request::parse_request;
use serde_json::json;

use super::findings::*;
use super::plan::*;
use super::resubmit::{ResubmitObs, create_and_supersede};

fn finding(id: &str, severity: Severity, origin: Origin, file: Option<&str>) -> ReviewFinding {
    ReviewFinding {
        id: id.to_string(),
        severity,
        origin,
        package: "curl".to_string(),
        file: file.map(str::to_string),
        line: None,
        message: format!("{id} message"),
    }
}

fn request_xml(state: &str, kind: &str, source_project: &str) -> String {
    format!(
        "<request id=\"1394662\" creator=\"alice\"><action type=\"{kind}\"><source project=\"{source_project}\" package=\"curl\" rev=\"abc\"/><target project=\"openSUSE:Factory\" package=\"curl\"/></action><state name=\"{state}\" who=\"alice\" when=\"2026-11-01T10:00:00\" created=\"2026-11-01T09:00:00\"/></request>"
    )
}

#[test]
fn a_saved_review_gives_its_findings_and_revision() {
    let text = json!({
        "schema": 1,
        "request": {"id": "1394662", "findings": [
            {"id": "request.competing", "severity": "warning", "origin": "check",
             "package": "curl", "file": null, "line": null, "message": "another"}
        ]},
        "actions": [{
            "source": {"project": "home:alice:branches:openSUSE:Factory", "package": "curl", "rev": "abc"},
            "findings": [
                {"id": "files.leftover", "severity": "warning", "origin": "check",
                 "package": "curl", "file": "curl.spec.orig", "line": 3, "message": "left"},
                {"id": "agent.finding", "severity": "note", "origin": "agent",
                 "package": "curl", "file": null, "line": null, "message": "opinion"}
            ]
        }]
    })
    .to_string();
    let saved = parse_saved(&text, "1394662").unwrap();
    assert_eq!(saved.rev.as_deref(), Some("abc"));
    assert_eq!(saved.findings.len(), 3);
    let leftover = &saved.findings[1];
    assert_eq!(leftover.id, "files.leftover");
    assert_eq!(leftover.file.as_deref(), Some("curl.spec.orig"));
    assert_eq!(leftover.line, Some(3));
    assert_eq!(saved.findings[2].origin, Origin::Agent);
}

#[test]
fn a_saved_review_of_another_request_or_schema_is_refused() {
    let other = json!({"schema": 1, "request": {"id": "7"}, "actions": []}).to_string();
    assert!(
        parse_saved(&other, "1394662")
            .unwrap_err()
            .contains("request 7")
    );
    let schema = json!({"schema": 2, "request": {"id": "1394662"}, "actions": []}).to_string();
    assert!(parse_saved(&schema, "1394662").is_err());
    let two = json!({"schema": 1, "request": {"id": "1394662"},
        "actions": [{"source": {}}, {"source": {}}]})
    .to_string();
    assert!(parse_saved(&two, "1394662").is_err());
    assert!(parse_saved("not json", "1394662").is_err());
}

#[test]
fn only_an_open_submit_request_from_the_users_branch_is_fixed() {
    let own = parse_request(&request_xml(
        "review",
        "submit",
        "home:alice:branches:openSUSE:Factory",
    ))
    .unwrap();
    let (source, target) = request_refusal(&own, "alice").unwrap();
    assert_eq!(source.project, "home:alice:branches:openSUSE:Factory");
    assert_eq!(target.project, "openSUSE:Factory");

    let declined = parse_request(&request_xml(
        "declined",
        "submit",
        "home:alice:branches:openSUSE:Factory",
    ))
    .unwrap();
    assert!(request_refusal(&declined, "alice").is_ok());

    let accepted = parse_request(&request_xml(
        "accepted",
        "submit",
        "home:alice:branches:openSUSE:Factory",
    ))
    .unwrap();
    assert_eq!(request_refusal(&accepted, "alice").unwrap_err().code, 3);

    let someone = parse_request(&request_xml(
        "review",
        "submit",
        "home:bob:branches:openSUSE:Factory",
    ))
    .unwrap();
    assert_eq!(request_refusal(&someone, "alice").unwrap_err().code, 4);

    let maint = parse_request(&request_xml(
        "review",
        "maintenance_incident",
        "home:alice:branches:openSUSE:Factory",
    ))
    .unwrap();
    assert_eq!(request_refusal(&maint, "alice").unwrap_err().code, 4);
}

#[test]
fn a_home_project_that_is_not_a_branch_is_refused() {
    let plain = parse_request(&request_xml("review", "submit", "home:alice:work")).unwrap();
    assert_eq!(request_refusal(&plain, "alice").unwrap_err().code, 4);
}

#[test]
fn a_request_by_someone_else_is_refused_even_from_the_users_branch() {
    // The branch is alice's, the request was made by bob.
    let by_bob = request_xml("review", "submit", "home:alice:branches:openSUSE:Factory")
        .replace("creator=\"alice\"", "creator=\"bob\"");
    let doc = parse_request(&by_bob).unwrap();
    let refusal = request_refusal(&doc, "alice").unwrap_err();
    assert_eq!(refusal.code, 4);
    assert!(
        refusal.message.contains("only its creator"),
        "{}",
        refusal.message
    );
}

#[test]
fn a_pinned_request_keeps_markup_in_its_values_as_text() {
    use ap_build::request_review::request::{ActionSource, ActionTarget};

    use super::request::pinned_xml;
    let mut doc = parse_request(&request_xml(
        "review",
        "submit",
        "home:alice:branches:openSUSE:Factory",
    ))
    .unwrap();
    doc.creator = "al\"ice&<x>".to_string();
    let source = ActionSource {
        project: "home:alice:branches:a&b\"c".to_string(),
        package: "curl".to_string(),
        rev: None,
    };
    let target = ActionTarget {
        project: "openSUSE:Factory".to_string(),
        package: "curl".to_string(),
    };
    let back = parse_request(&pinned_xml(&doc, &source, &target, "f&\"1")).unwrap();
    assert_eq!(back.creator, "al\"ice&<x>");
    let action = &back.actions[0];
    let pinned = ActionSource {
        rev: Some("f&\"1".to_string()),
        ..source
    };
    assert_eq!(action.source.as_ref().unwrap(), &pinned);
    assert_eq!(action.target.as_ref().unwrap(), &target);
}

#[test]
fn a_source_must_be_a_named_branch_of_the_user_and_not_the_target() {
    let refused = |xml: String| {
        let doc = parse_request(&xml).unwrap();
        request_refusal(&doc, "alice").unwrap_err().code
    };
    // The target is the source project itself.
    let same = request_xml("review", "submit", "home:alice:branches:openSUSE:Factory").replace(
        "target project=\"openSUSE:Factory\"",
        "target project=\"home:alice:branches:openSUSE:Factory\"",
    );
    assert_eq!(refused(same), 4);
    // Another user's name that starts with the same letters.
    assert_eq!(
        refused(request_xml(
            "review",
            "submit",
            "home:alicex:branches:openSUSE:Factory"
        )),
        4
    );
    // A branch prefix with nothing after it.
    assert_eq!(
        refused(request_xml("review", "submit", "home:alice:branches:")),
        4
    );
}

#[test]
fn a_saved_review_must_be_of_the_requests_package_project_and_revision() {
    let text = json!({"schema": 1, "request": {"id": "1394662"}, "actions": [{
        "source": {"project": "home:alice:branches:openSUSE:Factory", "package": "curl", "rev": "abc"},
        "findings": []
    }]})
    .to_string();
    let saved = parse_saved(&text, "1394662").unwrap();
    let own = parse_request(&request_xml(
        "review",
        "submit",
        "home:alice:branches:openSUSE:Factory",
    ))
    .unwrap();
    let (source, _) = request_refusal(&own, "alice").unwrap();
    assert!(saved_mismatch(&saved, &source).is_none());

    let mut other_package = source.clone();
    other_package.package = "wget".to_string();
    assert!(
        saved_mismatch(&saved, &other_package)
            .unwrap()
            .contains("wget")
    );

    let mut other_project = source.clone();
    other_project.project = "home:alice:branches:openSUSE:Leap".to_string();
    assert!(saved_mismatch(&saved, &other_project).is_some());

    let mut other_rev = source.clone();
    other_rev.rev = Some("def".to_string());
    assert!(
        saved_mismatch(&saved, &other_rev)
            .unwrap()
            .contains("revision")
    );
}

#[test]
fn the_delta_names_resolved_introduced_and_left_findings() {
    let before = vec![
        finding(
            "files.leftover",
            Severity::Warning,
            Origin::Check,
            Some("a.orig"),
        ),
        finding(
            "patch.header",
            Severity::Warning,
            Origin::Check,
            Some("0001.patch"),
        ),
    ];
    let after = vec![
        finding(
            "patch.header",
            Severity::Warning,
            Origin::Check,
            Some("0001.patch"),
        ),
        finding("build.pending", Severity::Warning, Origin::Check, None),
    ];
    let d = delta(&before, &after);
    assert_eq!(
        d.resolved.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["files.leftover"]
    );
    assert_eq!(
        d.left.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["patch.header"]
    );
    assert_eq!(
        d.introduced
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["build.pending"]
    );
}

#[test]
fn a_remaining_warning_for_the_tool_or_a_blocker_for_a_maintainer_stops_the_submit() {
    // A person's warning does not stop a request; the person decides with --yes.
    let person = [finding(
        "version.behind-upstream",
        Severity::Warning,
        Origin::Check,
        None,
    )];
    assert!(not_submittable(&person).is_none());
    assert_eq!(
        needs_person(&person).as_deref(),
        Some("a person must decide: version.behind-upstream")
    );

    let blocker = [finding(
        "source.signature",
        Severity::Blocker,
        Origin::Check,
        None,
    )];
    let refused = not_submittable(&blocker).unwrap();
    assert_eq!(refused.code, EXIT_NEEDS_MAINTAINER);

    let left = [finding(
        "patch.header",
        Severity::Warning,
        Origin::Check,
        Some("0001.patch"),
    )];
    let refused = not_submittable(&left).unwrap();
    assert_eq!(refused.code, 1);
    assert!(refused.message.contains("patch.header"));

    // A note and a finding of the agent never stop it.
    let notes = [
        finding("changes.missing-entry", Severity::Note, Origin::Check, None),
        finding("agent.finding", Severity::Blocker, Origin::Agent, None),
    ];
    assert!(not_submittable(&notes).is_none());
    assert!(needs_person(&notes).is_none());
}

#[test]
fn the_routes_of_the_fix_are_the_ones_the_tool_acts_on() {
    assert!(is_actionable(&FindingRoute::Agent));
    assert!(is_actionable(&FindingRoute::Wait));
    assert!(!is_actionable(&FindingRoute::Maintainer("x")));
    assert!(!is_actionable(&FindingRoute::NotActedOn));
}

#[test]
fn the_new_request_names_what_it_replaces_and_fixes() {
    let resolved = [
        finding(
            "files.leftover",
            Severity::Warning,
            Origin::Check,
            Some("a.orig"),
        ),
        finding(
            "files.leftover",
            Severity::Warning,
            Origin::Check,
            Some("b.orig"),
        ),
    ];
    let left = [
        finding(
            "version.behind-upstream",
            Severity::Warning,
            Origin::Check,
            None,
        ),
        finding("changes.header-format", Severity::Note, Origin::Check, None),
    ];
    assert_eq!(
        findings_description("1394662", &resolved, &left),
        "Supersedes request 1394662; fixes: files.leftover; remaining: version.behind-upstream."
    );
    assert_eq!(
        findings_description("1", &[], &[]),
        "Supersedes request 1; fixes: none; remaining: none."
    );
}

/// A fake of the writes of a re-submit: what was called, and each answer.
struct FakeWrites {
    calls: std::cell::RefCell<Vec<String>>,
    created: Result<String, String>,
    superseded: Result<(), String>,
}

impl FakeWrites {
    fn new(created: Result<String, String>, superseded: Result<(), String>) -> Self {
        FakeWrites {
            calls: Default::default(),
            created,
            superseded,
        }
    }
}

impl ResubmitObs for FakeWrites {
    fn create(&self, _sr: &osc::SubmitRequest) -> Result<String, String> {
        self.calls.borrow_mut().push("create".to_string());
        self.created.clone()
    }
    fn supersede(&self, old: &str, new: &str, _comment: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("supersede {old} {new}"));
        self.superseded.clone()
    }
}

fn a_request() -> osc::SubmitRequest {
    osc::SubmitRequest {
        source_project: "home:alice:branches:openSUSE:Factory".into(),
        source_package: "curl".into(),
        target_project: "openSUSE:Factory".into(),
        target_package: "curl".into(),
        description: "x".into(),
        source_rev: Some("abc".into()),
    }
}

#[test]
fn the_new_request_is_created_before_the_old_one_is_superseded() {
    let fake = FakeWrites::new(Ok("1394999".into()), Ok(()));
    let new = create_and_supersede(&fake, &a_request(), "1394662").unwrap();
    assert_eq!(new, "1394999");
    assert_eq!(
        *fake.calls.borrow(),
        ["create", "supersede 1394662 1394999"]
    );
}

#[test]
fn a_request_that_is_not_created_supersedes_nothing() {
    let fake = FakeWrites::new(Err("OBS refused".into()), Ok(()));
    let e = create_and_supersede(&fake, &a_request(), "1394662").unwrap_err();
    assert!(
        e.contains("could not create") && e.contains("OBS refused"),
        "{e}"
    );
    assert_eq!(*fake.calls.borrow(), ["create"]);
}

#[test]
fn a_failed_supersede_names_both_requests_and_leaves_the_old_one() {
    let fake = FakeWrites::new(Ok("1394999".into()), Err("HTTP 403".into()));
    let e = create_and_supersede(&fake, &a_request(), "1394662").unwrap_err();
    assert!(e.contains("1394999") && e.contains("1394662"), "{e}");
    assert!(e.contains("untouched") && e.contains("HTTP 403"), "{e}");
    assert_eq!(fake.calls.borrow().len(), 2);
}

fn parses(args: &[&str]) -> Result<crate::fix_cmd::FixArgs, String> {
    use clap::Parser;
    let mut argv = vec!["auto-package", "fix"];
    argv.extend_from_slice(args);
    match crate::Cli::try_parse_from(argv).map(|c| c.command) {
        Ok(crate::Commands::Fix(a)) => Ok(a),
        Ok(_) => Err("not fix".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[test]
fn the_request_mode_takes_the_id_and_refuses_the_bug_flags() {
    let a = parses(&["--request", "1394662", "--submit", "--yes"]).unwrap();
    assert_eq!(a.request.as_deref(), Some("1394662"));
    assert!(a.project.is_none() && a.only.is_empty());
    let a = parses(&["--request", "1394662", "--findings", "review.json"]).unwrap();
    assert!(a.findings.is_some());
    // Digits only; no bug flag with it; no findings file without it.
    assert!(parses(&["--request", "12a"]).is_err());
    assert!(parses(&["--request", "1", "--bug", "bsc#1"]).is_err());
    assert!(parses(&["--request", "1", "--project", "openSUSE:Factory"]).is_err());
    assert!(parses(&["--findings", "review.json"]).is_err());
    // The bug mode still needs its project and package.
    assert!(parses(&["--only", "curl", "--bug", "bsc#1"]).is_err());
    assert!(parses(&["--project", "openSUSE:Factory", "--bug", "bsc#1"]).is_err());
    assert!(
        parses(&[
            "--project",
            "openSUSE:Factory",
            "--only",
            "curl",
            "--bug",
            "bsc#1"
        ])
        .is_ok()
    );
}
