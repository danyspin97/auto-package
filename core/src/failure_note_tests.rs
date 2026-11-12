use crate::failure_note::FailureNote;

#[test]
fn an_empty_note_writes_no_fields() {
    let text = serde_json::to_string(&FailureNote::default()).unwrap();
    assert_eq!(text, "{}");
    let back: FailureNote = serde_json::from_str(&text).unwrap();
    assert_eq!(back, FailureNote::default());
}

#[test]
fn a_note_round_trips_and_writes_only_the_fields_it_has() {
    let only_action = FailureNote {
        detail: None,
        action: Some("run `osc login`".into()),
    };
    let text = serde_json::to_string(&only_action).unwrap();
    assert_eq!(text, r#"{"action":"run `osc login`"}"#);
    assert_eq!(
        serde_json::from_str::<FailureNote>(&text).unwrap(),
        only_action
    );

    let both = FailureNote {
        detail: Some("round 1: boom\nround 2: boom again".into()),
        action: Some("free some disk space".into()),
    };
    let text = serde_json::to_string(&both).unwrap();
    assert_eq!(serde_json::from_str::<FailureNote>(&text).unwrap(), both);
}

#[test]
fn a_needs_maintainer_outcome_carries_its_note_and_old_lines_still_parse() {
    use crate::event::PackageOutcome;
    let with = PackageOutcome::NeedsMaintainer {
        reason: "short".into(),
        note: Some(FailureNote {
            detail: Some("full".into()),
            action: None,
        }),
    };
    let text = serde_json::to_string(&with).unwrap();
    assert_eq!(
        text,
        r#"{"NeedsMaintainer":{"reason":"short","note":{"detail":"full"}}}"#
    );
    assert!(matches!(
        serde_json::from_str::<PackageOutcome>(&text).unwrap(),
        PackageOutcome::NeedsMaintainer { note: Some(n), .. } if n.detail.as_deref() == Some("full")
    ));
    // A line written before the note existed.
    let old = r#"{"NeedsMaintainer":{"reason":"gone"}}"#;
    assert!(matches!(
        serde_json::from_str::<PackageOutcome>(old).unwrap(),
        PackageOutcome::NeedsMaintainer { note: None, .. }
    ));
    // Without a note nothing extra is written.
    let none = PackageOutcome::NeedsMaintainer {
        reason: "gone".into(),
        note: None,
    };
    assert_eq!(serde_json::to_string(&none).unwrap(), old);
}
