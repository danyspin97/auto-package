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
