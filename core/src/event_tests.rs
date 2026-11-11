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

#[test]
fn a_finding_fix_event_round_trips_and_prints_one_line() {
    use crate::fix_event::FindingFixEvent;
    let f = FindingFixEvent {
        step: None,
        pkg: "p".into(),
        request: "1394662".into(),
        finding: "files.leftover".into(),
        route: "tool".into(),
        outcome: "fixed".into(),
        reason: "removed a.orig".into(),
    };
    let json = serde_json::to_string(&Event::FindingFix(f.clone())).unwrap();
    assert!(json.contains("\"event\":\"finding_fix\""), "{json}");
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Event::FindingFix(b) if b == f));
    assert_eq!(
        f.line(),
        "  request 1394662 finding files.leftover (tool): fixed: removed a.orig"
    );
}

#[test]
fn sweep_progress_names_its_package_and_old_logs_still_parse() {
    use crate::sweep_event::{SweepVerdict, SweptPackage};
    let e = Event::SweepProgress {
        checked: 1,
        to_update: 1,
        up_to_date: 0,
        skipped: 0,
        finished: false,
        package: Some(SweptPackage {
            name: "python-foo".into(),
            obs_name: String::new(),
            verdict: SweepVerdict::Update {
                from: "1.0".into(),
                to: "2.0".into(),
            },
        }),
        total: None,
    };
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"event":"sweep_progress","checked":1,"to_update":1,"up_to_date":0,"skipped":0,"finished":false,"package":{"name":"python-foo","verdict":{"kind":"update","from":"1.0","to":"2.0"}}}"#
    );
    let old: Event = serde_json::from_str(
        r#"{"event":"sweep_progress","checked":3,"to_update":0,"up_to_date":3,"skipped":0}"#,
    )
    .unwrap();
    assert!(matches!(
        old,
        Event::SweepProgress {
            checked: 3,
            finished: false,
            package: None,
            ..
        }
    ));
}

#[test]
fn sweep_progress_carries_its_total_when_known() {
    let e = Event::SweepProgress {
        checked: 2,
        to_update: 0,
        up_to_date: 2,
        skipped: 0,
        finished: false,
        package: None,
        total: Some(120),
    };
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.contains(r#""total":120"#), "{json}");
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        back,
        Event::SweepProgress {
            checked: 2,
            total: Some(120),
            ..
        }
    ));
    // Without a total the key is not written, and an old line gives none.
    let none = Event::SweepProgress {
        checked: 2,
        to_update: 0,
        up_to_date: 2,
        skipped: 0,
        finished: false,
        package: None,
        total: None,
    };
    let json = serde_json::to_string(&none).unwrap();
    assert!(!json.contains("total"), "{json}");
    let old: Event = serde_json::from_str(
        r#"{"event":"sweep_progress","checked":3,"to_update":0,"up_to_date":3,"skipped":0,"finished":true}"#,
    )
    .unwrap();
    assert!(matches!(old, Event::SweepProgress { total: None, .. }));
}

#[test]
fn sweep_checking_names_its_package_and_the_check_its_name() {
    use crate::sweep_event::{SweepVerdict, SweptPackage};
    let e = Event::SweepChecking {
        package: "python-APScheduler".into(),
    };
    let json = serde_json::to_string(&e).unwrap();
    assert_eq!(
        json,
        r#"{"event":"sweep_checking","package":"python-APScheduler"}"#
    );
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Event::SweepChecking { package } if package == "python-APScheduler"));
    // A result for a build's row names the OBS name it was checked under.
    let result = SweptPackage {
        name: "python-apscheduler".into(),
        obs_name: "python-APScheduler".into(),
        verdict: SweepVerdict::UpToDate {
            version: "3.10".into(),
        },
    };
    let json = serde_json::to_string(&result).unwrap();
    assert!(
        json.contains(r#""obs_name":"python-APScheduler""#),
        "{json}"
    );
    // Older logs have no OBS name.
    let old: SweptPackage = serde_json::from_str(
        r#"{"name":"python-foo","verdict":{"kind":"up_to_date","version":"1"}}"#,
    )
    .unwrap();
    assert_eq!(old.obs_name, "");
    let no_name = SweptPackage {
        obs_name: String::new(),
        ..result
    };
    let json = serde_json::to_string(&no_name).unwrap();
    assert!(!json.contains("obs_name"), "{json}");
}

#[test]
fn a_package_finished_line_without_a_remedy_still_parses() {
    let old = r#"{"event":"package_finished","pkg":"python-foo","version":"1.0","outcome":{"Failed":{"reason":"boom","disk_full":false}}}"#;
    let e: Event = serde_json::from_str(old).unwrap();
    assert!(matches!(
        e,
        Event::PackageFinished {
            outcome: PackageOutcome::Failed {
                remedy: None,
                disk_full: false,
                ..
            },
            ..
        }
    ));
}

#[test]
fn a_failure_remedy_round_trips_and_an_absent_one_is_not_written() {
    use crate::remedy::{Cause, Remedy, Retry};
    let with = Event::PackageFinished {
        pkg: "python-foo".into(),
        version: "1.0".into(),
        outcome: PackageOutcome::Failed {
            reason: "osc is missing".into(),
            disk_full: false,
            remedy: Some(Remedy {
                cause: Cause::MissingTool {
                    tool: "osc".into(),
                    path: None,
                },
                retry: Retry::SameCommand,
            }),
            note: None,
        },
    };
    let text = serde_json::to_string(&with).unwrap();
    assert!(text.contains(
        r#""remedy":{"cause":{"kind":"missing_tool","tool":"osc","path":null},"retry":"same_command"}"#
    ));
    let back: Event = serde_json::from_str(&text).unwrap();
    assert!(matches!(
        back,
        Event::PackageFinished {
            outcome: PackageOutcome::Failed {
                remedy: Some(Remedy {
                    retry: Retry::SameCommand,
                    ..
                }),
                ..
            },
            ..
        }
    ));

    let without = Event::PackageFinished {
        pkg: "python-foo".into(),
        version: "1.0".into(),
        outcome: PackageOutcome::Failed {
            reason: "boom".into(),
            disk_full: false,
            remedy: None,
            note: None,
        },
    };
    let text = serde_json::to_string(&without).unwrap();
    assert!(!text.contains("remedy"), "{text}");
}

#[test]
fn a_failure_note_round_trips_and_an_absent_one_is_not_written() {
    use crate::failure_note::FailureNote;
    let note = FailureNote {
        detail: Some("round 1: boom\nround 2: boom".into()),
        action: Some("free some disk space".into()),
    };
    let with = Event::PackageFinished {
        pkg: "python-foo".into(),
        version: "1.0".into(),
        outcome: PackageOutcome::Failed {
            reason: "boom".into(),
            disk_full: false,
            remedy: None,
            note: Some(note.clone()),
        },
    };
    let text = serde_json::to_string(&with).unwrap();
    assert!(text.contains(
        r#""note":{"detail":"round 1: boom\nround 2: boom","action":"free some disk space"}"#
    ));
    let back: Event = serde_json::from_str(&text).unwrap();
    assert!(matches!(
        back,
        Event::PackageFinished {
            outcome: PackageOutcome::Failed {
                note: Some(ref got),
                ..
            },
            ..
        } if *got == note
    ));

    let without = Event::PackageFinished {
        pkg: "python-foo".into(),
        version: "1.0".into(),
        outcome: PackageOutcome::Failed {
            reason: "boom".into(),
            disk_full: false,
            remedy: None,
            note: None,
        },
    };
    let text = serde_json::to_string(&without).unwrap();
    assert!(!text.contains("note"), "{text}");
}

#[test]
fn a_package_finished_line_without_a_note_still_parses() {
    let old = r#"{"event":"package_finished","pkg":"python-foo","version":"1.0","outcome":{"Failed":{"reason":"boom","disk_full":false,"remedy":null}}}"#;
    let e: Event = serde_json::from_str(old).unwrap();
    assert!(matches!(
        e,
        Event::PackageFinished {
            outcome: PackageOutcome::Failed { note: None, .. },
            ..
        }
    ));
}
