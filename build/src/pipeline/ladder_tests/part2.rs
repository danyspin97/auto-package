use super::*;
use crate::pipeline::logs::*;

#[test]
fn a_spent_ladder_fails_exhausted_with_every_reason_and_is_remembered() {
    let lookups = std::cell::Cell::new(0);
    let case = run_case(
        "python-foo",
        false,
        &[],
        blocked("reason zero"),
        vec![
            blocked("reason one"),
            blocked("reason two"),
            blocked("reason three"),
        ],
        &lookups,
        None,
    );
    // The wheel rung of round 2 was tried (and failed) without a turn.
    assert_eq!(lookups.get(), 1);
    assert_eq!(case.contexts.len(), 3);
    match case.result {
        Err(PipelineResult::Failed { reason, kind, .. }) => {
            assert_eq!(kind, FailureKind::Exhausted);
            assert!(reason.starts_with(LADDER_FAILED_PREFIX), "{reason}");
            for r in ["reason zero", "reason one", "reason two", "reason three"] {
                assert!(
                    reason.contains(r) && case.notes.contains(r),
                    "{r}: {reason} {}",
                    case.notes
                );
            }
        }
        other => panic!("{other:?}"),
    }
    // The memo holds the spec: the same spec is not tried again.
    let ws = Workspace::open(&case.state_root, "python-foo").unwrap();
    let state = PkgState::load(&ws);
    assert_eq!(state.ladder_spec_hash, Some(case.config_key));
    assert!(state.ladder_reason.unwrap().contains("reason three"));
}

fn ctx_for<'a>(
    config: &'a PipelineConfig,
    pkg_run: &'a ap_core::run::PkgRunDir,
    state_root: &Path,
) -> RunCtx<'a> {
    RunCtx {
        config,
        pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws: Workspace::open(state_root, "python-foo").unwrap(),
        rpm_name: "python-foo".into(),
        obs_name: "python-foo".into(),
        checkout_dir: PathBuf::new(),
        spec_path: PathBuf::new(),
    }
}

#[test]
fn the_memo_refuses_a_repeat_unless_forced_patching_or_the_spec_changed() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.work_dir = root.path().join("work");
    config.project = "p".into();
    let ws = Workspace::open(&state_root, "python-foo").unwrap();
    let run = ap_core::run::RunDir::allocate(root.path()).unwrap();
    let pkg_run = run.pkg("python-foo");
    let spec = "Name: python-foo\n";
    let key = ladder_key(&config, spec);
    update_state(&ws, &ap_core::sink::NULL_REPORTER, |s| {
        s.ladder_spec_hash = Some(key);
        s.ladder_reason = Some("turn 0: nope".into());
    });
    let refused = spent_ladder_refusal(&ctx_for(&config, &pkg_run, &state_root), false, spec);
    match refused {
        Some(PipelineResult::Failed { reason, kind, .. }) => {
            assert_eq!(kind, FailureKind::Exhausted);
            assert!(reason.contains("not trying again without --force") && reason.contains("nope"));
        }
        other => panic!("{other:?}"),
    }
    // A changed spec, a package being patched for a dependent: asked again.
    assert!(
        spent_ladder_refusal(
            &ctx_for(&config, &pkg_run, &state_root),
            false,
            "Name: python-foo\nVersion: 2\n"
        )
        .is_none()
    );
    assert!(spent_ladder_refusal(&ctx_for(&config, &pkg_run, &state_root), true, spec).is_none());
    // --force bypasses the memo.
    config.force = true;
    assert!(spent_ladder_refusal(&ctx_for(&config, &pkg_run, &state_root), false, spec).is_none());
}

#[test]
fn only_failing_check_tests_are_skipped_with_evidence_without_a_turn() {
    let lookups = std::cell::Cell::new(0);
    let log = "[  30s] FAILED tests/test_a.py::test_one - AssertionError: boom\n[  30s] ===== 1 failed, 40 passed in 2s =====\n";
    let message = changes_message("Skip the failing tests tests/test_a.py::test_one");
    let case = run_case(
        "mathlib",
        false,
        &[],
        blocked("test_one fails and I may not deselect it"),
        Vec::new(),
        &lookups,
        Some((log, &message)),
    );
    assert_eq!(case.result.unwrap(), TurnEffect::Edited);
    assert!(case.contexts.is_empty());
    assert!(
        case.notes.contains("failing tests skipped"),
        "{}",
        case.notes
    );
}

#[test]
fn a_check_import_error_is_not_dropped_and_the_ladder_goes_on_to_a_turn() {
    let lookups = std::cell::Cell::new(0);
    let log = "[  30s] FAILED tests/test_a.py::test_one - ModuleNotFoundError: No module named 'numpy'\n[  30s] ===== 1 failed, 40 passed in 2s =====\n";
    let case = run_case(
        "mathlib",
        false,
        &[],
        blocked("needs numpy"),
        (0..3)
            .map(|_| scripted_outcome(crate::agent::Action::NoChange, "x"))
            .collect(),
        &lookups,
        Some((log, "unused")),
    );
    // The turn was asked, and the refusal is in the notes.
    assert!(!case.contexts.is_empty());
    assert!(
        case.notes.contains("failing tests not skipped"),
        "{}",
        case.notes
    );
    assert!(case.notes.contains("dependency problem"), "{}", case.notes);
}

#[test]
fn the_drop_edit_needs_a_check_failure() {
    let log = "FAILED tests/t.py::test_a - assert 0\n== 1 failed, 9 passed in 1s ==\n";
    let none = |_: &str| false;
    assert!(dropped_tests_edit(Some("%build"), log, "%check\n%pytest\n", &none).is_err());
    assert!(dropped_tests_edit(None, log, "%check\n%pytest\n", &none).is_err());
    assert!(dropped_tests_edit(Some("%check"), log, "%check\n%pytest\n", &none).is_ok());
}
