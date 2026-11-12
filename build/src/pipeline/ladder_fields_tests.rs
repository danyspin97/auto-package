use super::*;
use ap_core::failure_note::FailureNote;

#[test]
fn an_old_state_without_the_recap_keys_still_loads() {
    let old = r#"{"last_outcome":"failed","ladder_spec_hash":7,"ladder_reason":"turn 0: x | round 1: y","ladder_needs_maintainer":true}"#;
    let st: PkgState = serde_json::from_str(old).unwrap();
    assert_eq!(st.ladder_spec_hash, Some(7));
    assert_eq!(st.ladder_reason.as_deref(), Some("turn 0: x | round 1: y"));
    assert!(st.ladder_needs_maintainer);
    assert_eq!(st.ladder_recap, LadderRecapFields::default());
}

#[test]
fn the_recap_keys_are_flat_in_the_state_json_and_round_trip() {
    let st = PkgState {
        ladder_reason: Some("full".into()),
        ladder_recap: LadderRecapFields {
            ladder_summary: Some("short".into()),
            ladder_action: Some("do this".into()),
        },
        ..Default::default()
    };
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["ladder_summary"], "short");
    assert_eq!(json["ladder_action"], "do this");
    assert!(json.get("ladder_recap").is_none(), "{json}");
    let back: PkgState = serde_json::from_value(json).unwrap();
    assert_eq!(back.ladder_recap, st.ladder_recap);
    // Without a recap the keys are not written at all.
    let json = serde_json::to_value(PkgState::default()).unwrap();
    assert!(json.get("ladder_summary").is_none() && json.get("ladder_action").is_none());
}

#[test]
fn a_recorded_build_clears_the_recap_with_the_memo() {
    let mut st = PkgState {
        ladder_recap: LadderRecapFields {
            ladder_summary: Some("short".into()),
            ladder_action: None,
        },
        ..Default::default()
    };
    st.record_built("1.0", false);
    assert_eq!(st.ladder_recap, LadderRecapFields::default());
}

#[test]
fn clearing_the_memo_removes_the_recap_with_it() {
    let mut st = PkgState {
        ladder_spec_hash: Some(7),
        ladder_reason: Some("turn 0: x".into()),
        ladder_needs_maintainer: true,
        ladder_recap: LadderRecapFields {
            ladder_summary: Some("short".into()),
            ladder_action: Some("do this".into()),
        },
        ..Default::default()
    };
    ladder_fields::clear_ladder_memo(&mut st);
    assert_eq!(st.ladder_spec_hash, None);
    assert_eq!(st.ladder_reason, None);
    assert!(!st.ladder_needs_maintainer);
    assert_eq!(st.ladder_recap, LadderRecapFields::default());
}

#[test]
fn a_failure_can_carry_a_note() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.work_dir = root.path().join("work");
    config.project = "p".into();
    let run = ap_core::run::RunDir::allocate(root.path()).unwrap();
    let pkg_run = run.pkg("foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws: Workspace::open(&state_root, "foo").unwrap(),
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: PathBuf::new(),
        spec_path: PathBuf::new(),
        signals: Default::default(),
    };
    let note = FailureNote {
        detail: Some("all of it".into()),
        action: Some("do this".into()),
    };
    match ctx.failed_note(FailureKind::Exhausted, "short".into(), note.clone()) {
        PipelineResult::Failed {
            rpm_name,
            reason,
            kind,
            remedy,
            note: got,
        } => {
            assert_eq!((rpm_name.as_str(), reason.as_str()), ("foo", "short"));
            assert_eq!(kind, FailureKind::Exhausted);
            assert!(remedy.is_none());
            assert_eq!(got, Some(note));
        }
        other => panic!("{other:?}"),
    }
}
