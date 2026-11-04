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
