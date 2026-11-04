use crate::event::*;

#[test]
fn host_allowed_has_its_wire_form_and_defaults_when_fields_are_missing() {
    let out = serde_json::to_string(&Event::HostAllowed {
        host: "x.example.org".into(),
        scope: HostScope::Persistent,
        source: HostSource::Tui,
    })
    .unwrap();
    assert_eq!(
        out,
        r#"{"event":"host_allowed","host":"x.example.org","scope":"persistent","source":"tui"}"#
    );
    let e: Event =
        serde_json::from_str(r#"{"event":"host_allowed","host":"a.example.org"}"#).unwrap();
    assert!(matches!(
        e,
        Event::HostAllowed {
            ref host,
            scope: HostScope::Run,
            source: HostSource::Flag,
        } if host == "a.example.org"
    ));
}

#[test]
fn arch_check_has_its_wire_form_and_old_logs_still_parse() {
    let e = Event::ArchCheck {
        package: "foo".into(),
        results: vec![ArchCheckResult {
            repo: "ports".into(),
            arch: "s390x".into(),
            state: "failed".into(),
            detail: None,
            origin_fails: false,
            origin_missing: false,
            log: None,
        }],
    };
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"event":"arch_check","package":"foo","results":[{"repo":"ports","arch":"s390x","state":"failed","origin_fails":false}]}"#
    );
    let old: Event = serde_json::from_str(r#"{"event":"arch_check","package":"foo"}"#).unwrap();
    assert!(matches!(old, Event::ArchCheck { ref results, .. } if results.is_empty()));
    let bare: ArchCheckResult =
        serde_json::from_str(r#"{"repo":"ports","arch":"s390x","state":"succeeded"}"#).unwrap();
    assert!(!bare.origin_fails && bare.detail.is_none() && bare.log.is_none());
}

#[test]
fn bug_fix_has_its_wire_form_and_a_line_for_each_stage() {
    use crate::fix_event::{BugFixEvent, BugFixStage};
    let input = BugFixEvent {
        step: None,
        pkg: "foo".into(),
        stage: BugFixStage::Input,
        refs: vec!["bsc#1234567".into(), "CVE-2025-1234".into()],
        read: vec!["api.osv.dev CVE-2025-1234".into()],
        not_read: vec!["bsc#1234567: not public".into()],
        patch: Some("foo-CVE-2025-1234.patch".into()),
        patch_sha256: None,
        origin: None,
        verdict: None,
        applied: None,
        reason: String::new(),
    };
    let e = Event::BugFix(input.clone());
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"event":"bug_fix","pkg":"foo","stage":"input","refs":["bsc#1234567","CVE-2025-1234"],"read":["api.osv.dev CVE-2025-1234"],"not_read":["bsc#1234567: not public"],"patch":"foo-CVE-2025-1234.patch","reason":""}"#
    );
    let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
    assert!(matches!(back, Event::BugFix(b) if b == input));
    assert_eq!(
        input.line(),
        "  bug fix input: bsc#1234567 CVE-2025-1234, staged foo-CVE-2025-1234.patch (1 not read)"
    );
    let end = BugFixEvent {
        stage: BugFixStage::End,
        verdict: Some("fixed".into()),
        applied: Some("clean".into()),
        reason: "applies".into(),
        ..input
    };
    assert_eq!(end.line(), "  bug fix fixed (clean): applies");
    assert_eq!(Task::FixBug.to_string(), "fix bug");
}
