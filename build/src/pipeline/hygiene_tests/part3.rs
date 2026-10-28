use super::*;

#[test]
fn failures_counted_while_unsaved_join_the_stored_count() {
    let key = condition_key(&utils_condition());
    let mut state = PkgState::default();
    // Nothing unsaved: the first failure is 1.
    assert_eq!(state.record_condition_failure_after(key, 0), 1);
    // Two unsaved failures on top of the one stored: 1 + 2 + 1.
    assert_eq!(state.record_condition_failure_after(key, 2), 4);
    assert_eq!(state.condition_failures_of(key), 4);
}

#[test]
fn an_unsaved_failure_count_adds_to_the_stored_one() {
    // Stored 1, this failure unsaved: 2, so the cap is reached.
    assert_eq!(count_when_unsaved(Ok(1), 1), 2);
    // Stored count unreadable: the unsaved failures alone.
    assert_eq!(
        count_when_unsaved(Err(std::io::Error::other("locked")), 1),
        1
    );
}

#[test]
fn a_project_lookup_failure_is_absent_only_when_the_project_says_so() {
    #[derive(Default)]
    struct Warnings(std::sync::Mutex<usize>);
    impl Reporter for Warnings {
        fn report(&self, event: &Event) {
            if matches!(event, Event::Warning { .. }) {
                *self.0.lock().unwrap() += 1;
            }
        }
    }
    let rec = Warnings::default();
    let lookup = |reason: &str, path_project: bool| -> Result<Vec<String>, BranchError> {
        lookup_failure(
            reason.to_string(),
            osc::text_is_transient(reason),
            path_project,
            Emit::run(&rec),
        )
    };
    let is_failed = |r: &Result<Vec<String>, BranchError>, transient: bool| matches!(r, Err(BranchError::Failed { transient: t, .. }) if *t == transient);

    // A 404 means the project does not carry the package, for our own project too.
    let r = lookup("osc ls exited 1: HTTP Error 404: Not Found", false);
    assert!(matches!(r, Ok(ref v) if v.is_empty()));
    assert_eq!(*rec.0.lock().unwrap(), 1);
    // A 403 means so only for a path project; our own 403 is a failure.
    let r = lookup("osc ls exited 1: HTTP Error 403: Forbidden", true);
    assert!(matches!(r, Ok(ref v) if v.is_empty()));
    assert_eq!(*rec.0.lock().unwrap(), 2);
    let r = lookup("osc ls exited 1: HTTP Error 403: Forbidden", false);
    assert!(is_failed(&r, false));
    assert_eq!(*rec.0.lock().unwrap(), 2);
    // An unknown project is absent too.
    assert!(matches!(lookup("unknown project foo", false), Ok(ref v) if v.is_empty()));

    // A 401, an "unauthorized" text, an unrecognised error: all failures.
    for reason in [
        "osc ls exited 1: HTTP Error 401: Unauthorized",
        "osc ls exited 1: unauthorized: check your credentials",
        "osc ls exited 1: something nobody has seen before",
    ] {
        assert!(is_failed(&lookup(reason, true), false), "{reason}");
        assert!(is_failed(&lookup(reason, false), false), "{reason}");
    }
    // A failed launch of osc is a failure, not an absent project.
    let spawn = osc::Error::Launch(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No such file or directory (os error 2)",
    ));
    let r: Result<Vec<String>, BranchError> = lookup_failure(
        spawn.to_string(),
        spawn.is_transient(),
        true,
        Emit::run(&rec),
    );
    assert!(is_failed(&r, false));
    // A 503 or a timeout fails the stage as transient.
    for reason in [
        "osc ls exited 1: HTTP Error 503: Service Unavailable",
        "timed out",
    ] {
        assert!(is_failed(&lookup(reason, true), true), "{reason}");
    }
}

#[test]
fn a_provider_name_must_be_a_plain_package_name() {
    for ok in ["nvidia-nccl-cu13", "python313-foo", "a.b_c+d", "9lives"] {
        assert!(is_provider_stem(ok), "{ok}");
    }
    for bad in [
        "",
        "-foo",
        ".foo",
        "foo bar",
        "foo}\nBuildRequires: x",
        "a%{x}",
        "foo/bar",
    ] {
        assert!(!is_provider_stem(bad), "{bad:?}");
    }
}

#[test]
fn the_first_unmet_condition_fails_the_patch_and_the_second_switches_to_the_wheel() {
    let key = condition_key(&utils_condition());
    // First: the patch failed, counted, the recipe untouched.
    let (result, state, spec, _root) = gate_case(SPLIT_SPEC, 0, true);
    match result {
        Err(PipelineResult::Failed { reason, kind, .. }) => {
            assert_eq!(kind, FailureKind::ConditionUnmet);
            assert!(
                reason.contains("foo/utils") && reason.contains("python-foo-devel")
                    || reason.contains("devel"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(state.condition_failures_of(key), 1);
    assert_eq!(spec, SPLIT_SPEC);
    // Second: the package goes to its wheel, which settles the request.
    let (result, state, spec, _root) = gate_case(SPLIT_SPEC, 1, true);
    assert!(result.is_ok(), "{:?}", result.err());
    assert!(spec.contains("foo-1.0-py3-none-any.whl"), "{spec}");
    assert!(state.satisfied_conditions.contains(&key));
    assert_eq!(state.condition_failures_of(key), 0);
    // Second, and PyPI has no usable wheel: spent, with both reasons.
    let (result, _state, spec, _root) = gate_case(SPLIT_SPEC, 1, false);
    match result {
        Err(PipelineResult::Failed { reason, kind, .. }) => {
            assert_eq!(kind, FailureKind::Exhausted);
            assert!(
                reason.contains("2 patches") && reason.contains("wheel"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(spec, SPLIT_SPEC);
}

#[test]
fn a_failed_wheel_lookup_leaves_the_spec_to_the_review_turn() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkout = root.path().join("proj").join("python-foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let config = config("foo", &state_root);
    let ws = Workspace::open(&state_root, "python-foo").unwrap();
    for d in ["pybind11", "XNNPACK"] {
        std::fs::create_dir_all(ws.sdist_dir().join("foo-1.0/third_party").join(d)).unwrap();
    }
    let spec_path = checkout.join("python-foo.spec");
    let original = "Name: python-foo\nVersion: 1.0\n";
    std::fs::write(&spec_path, original).unwrap();
    let run = ap_core::run::RunDir::allocate(&root.path().join("work")).unwrap();
    let pkg_run = run.pkg("python-foo");
    let warnings = Warnings(Default::default());
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &warnings,
        ws,
        rpm_name: "python-foo".into(),
        obs_name: "python-foo".into(),
        checkout_dir: checkout,
        spec_path: spec_path.clone(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: "foo-1.0".into(),
        from_repo_archive: true,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
    };
    let offline = |_: &PipelineConfig| Err("PyPI fetch failed: offline".to_string());
    let switched = switch_for_missing_submodules(&ctx, &mut st, &spec_path, &offline)
        .unwrap_or_else(|_| panic!("the switch step failed"));
    assert!(!switched);
    assert_eq!(std::fs::read_to_string(&spec_path).unwrap(), original);
    assert!(st.from_repo_archive && st.sdist_top_level == "foo-1.0");
    assert!(
        warnings
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("the Review turn decides")),
        "{:?}",
        warnings.0.lock().unwrap()
    );
}
