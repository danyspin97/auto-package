//! The refusal to run a spent ladder again: the stored recap and the hint.

use super::*;

type Refusal = (String, FailureKind, ap_core::failure_note::FailureNote);

/// The refusal for a spec whose memo is `set`, as `python-foo` of the
/// package `foo`.
fn refusal(set: impl FnOnce(&mut PkgState)) -> Refusal {
    refusal_of("python-foo", "foo", None, set)
}

/// The refusal of package `pkg` (RPM name `rpm`, requested as `request`).
fn refusal_of(
    rpm: &str,
    pkg: &str,
    request: Option<&str>,
    set: impl FnOnce(&mut PkgState),
) -> Refusal {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut config = super::super::super::hygiene_tests::config(pkg, &state_root);
    config.work_dir = root.path().join("work");
    config.project = "p".into();
    config.request_text = request.map(str::to_string);
    let ws = Workspace::open(&state_root, rpm).unwrap();
    let run = ap_core::run::RunDir::allocate(root.path()).unwrap();
    let pkg_run = run.pkg(rpm);
    let spec = format!("Name: {rpm}\n");
    let key = ladder_key(&config, &spec);
    update_state(&ws, &ap_core::sink::NULL_REPORTER, |s| {
        s.ladder_spec_hash = Some(key);
        set(s);
    });
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: rpm.into(),
        obs_name: rpm.into(),
        checkout_dir: PathBuf::new(),
        spec_path: PathBuf::new(),
        signals: Default::default(),
    };
    match spent_ladder_refusal(&ctx, false, &spec) {
        Some(PipelineResult::Failed {
            reason,
            kind,
            note: Some(note),
            ..
        }) => (reason, kind, note),
        other => panic!("{other:?}"),
    }
}

/// What the old refusal showed for the jay package: the history stored before
/// the recap was kept, as `ladder_reason` held it.
const JAY_STORED: &str = "turn 0: The cargo_vendor service cannot run because the host's rustup \
    has no default toolchain (\"rustup could not choose a version of cargo to run ... run \
    'rustup default stable'\"). As a result vendor.tar.zst never exists, and only a fix to the \
    host environment can create it. | round 1: round 1 answered no_change: The only missing \
    source is Source1, vendor.tar.zst. The cargo_vendor service in _service (src=jay, \
    compression=zst) is what creates it. That service fails on the host before it vendors \
    anything: \"rustup could not choose a version of cargo to run ... no default is \
    configured\". The spec is consistent: ... | round 2: round 2 answered no_change: New this \
    round: I ruled out each alternative route. (1) stage_files: ... (3) Moving cargo_vendor to \
    server mode: convention 77 (section 92) says every service runs in manual mode, ... | \
    round 3: round 3 answered no_change: New this round: confirmed the checkout contains the \
    jay/ git clone ... Tried osc cat openSUSE:Factory jay _service ...; osc failed with a \
    keyring/DBus credential error ...";

const JAY_REASON: &str = "jay-compositor could not be resolved (the same spec failed before): \
    The cargo_vendor service cannot run because the host's rustup has no default toolchain. 3 \
    resolution rounds did not fix it.";

const JAY_ACTION: &str = "after fixing the cause, run: auto-package build jay-compositor --force";

#[test]
fn the_legacy_jay_memo_is_one_short_line_naming_the_cause() {
    let (reason, kind, note) = refusal_of("jay-compositor", "jay-compositor", None, |s| {
        s.ladder_reason = Some(JAY_STORED.into());
    });
    assert_eq!(kind, FailureKind::Exhausted);
    assert_eq!(reason, JAY_REASON);
    for gone in [
        "convention",
        "round 1",
        "answered",
        "UNRESOLVED",
        "investigate the tool",
        "rustup default stable",
    ] {
        assert!(!reason.contains(gone), "{gone}: {reason}");
    }
    // Only the force hint: the quoted advice in turn 0 is not an action.
    assert_eq!(note.action.as_deref(), Some(JAY_ACTION));
    let detail = note.detail.unwrap();
    assert!(detail.contains("convention 77 (section 92)"), "{detail}");
}

#[test]
fn a_legacy_memo_shown_in_the_old_refusal_wrapper_is_unwrapped_first() {
    let wrapped = format!(
        "{LADDER_FAILED_PREFIX}jay-compositor: the resolution ladder was spent on this spec \
         earlier and neither it nor the packages it depends on changed ({JAY_STORED}); not \
         trying again without --force"
    );
    let (reason, _, note) = refusal_of("jay-compositor", "jay-compositor", None, |s| {
        s.ladder_reason = Some(wrapped);
    });
    assert_eq!(reason, JAY_REASON);
    assert_eq!(note.action.as_deref(), Some(JAY_ACTION));
}

#[test]
fn the_hint_repeats_the_request_text_when_there_is_one() {
    let (_, _, note) = refusal_of("python-foo", "foo", Some("pypi:foo"), |s| {
        s.ladder_reason = Some("turn 0: x".into());
    });
    assert_eq!(
        note.action.as_deref(),
        Some("after fixing the cause, run: auto-package build pypi:foo --force")
    );
}

#[test]
fn a_stored_recap_and_action_replace_the_replay_of_the_history() {
    let (reason, kind, note) = refusal(|s| {
        s.ladder_reason =
            Some("turn 0: raw convention 77 (section 92) text | round 1: more".into());
        s.ladder_recap.ladder_summary = Some("The host has no Rust toolchain.".into());
        s.ladder_recap.ladder_action = Some("Run rustup default stable.".into());
    });
    assert_eq!(kind, FailureKind::Exhausted);
    assert_eq!(
        reason,
        "python-foo could not be resolved (the same spec failed before): The host has no Rust \
         toolchain."
    );
    // The package as the user types it, not the `python-` rpm name.
    assert_eq!(
        note.action.as_deref(),
        Some(
            "Run rustup default stable; after fixing the cause, run: auto-package build foo --force"
        )
    );
    let detail = note.detail.unwrap();
    assert!(detail.starts_with(LADDER_FAILED_PREFIX), "{detail}");
    assert!(detail.contains("turn 0: raw convention 77 (section 92) text | round 1: more"));
    assert!(detail.contains("not trying again without --force"));
}

#[test]
fn a_stored_summary_without_an_action_leaves_the_force_command_alone() {
    let (reason, _, note) = refusal(|s| {
        s.ladder_reason = Some("full".into());
        s.ladder_recap.ladder_summary = Some("Short.".into());
    });
    assert!(reason.ends_with(": Short."), "{reason}");
    assert!(!reason.contains("--force"), "{reason}");
    assert_eq!(
        note.action.as_deref(),
        Some("after fixing the cause, run: auto-package build foo --force")
    );
}

#[test]
fn a_memo_without_any_text_still_refuses_with_the_hint() {
    let (reason, kind, note) = refusal(|s| s.ladder_needs_maintainer = true);
    assert_eq!(kind, FailureKind::NeedsMaintainer);
    assert_eq!(
        reason,
        "python-foo could not be resolved (the same spec failed before):"
    );
    assert!(note.action.unwrap().contains("--force"));
}
