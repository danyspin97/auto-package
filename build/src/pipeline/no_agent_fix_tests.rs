//! `--no-agent`: a failed build still gets the fix that needs no agent.

use super::*;

const SPEC: &str = "Name: jay\nVersion: 1\nRelease: 0\nSummary: x\nLicense: MIT\n\n\
    %description\nx\n\n%files\n%{_bindir}/jay\n%{_datadir}/jay/jay.conf\n";

const UNOWNED: &str = "\
[   27s] Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.bbb
[   50s] ... running 50-check-filelist
[   50s] jay-1-0.x86_64.rpm: directories not owned by a package:
[   50s]  - /usr/share/jay
";

/// Runs `fix_after_build` with `--no-agent` on `log`; returns its result and
/// the spec afterwards.
fn run(log: &str) -> (Result<RoundEnd, PipelineResult>, String) {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("jay", &state_root);
    config.rpm_name = "jay".into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.no_agent = true;
    let checkout = work.join("proj").join("jay");
    std::fs::create_dir_all(&checkout).unwrap();
    let spec_path = checkout.join("jay.spec");
    std::fs::write(&spec_path, SPEC).unwrap();
    let build_log = root.path().join("build.log");
    std::fs::write(&build_log, log).unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("jay");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws: Workspace::open(&state_root, "jay").unwrap(),
        rpm_name: "jay".into(),
        obs_name: "jay".into(),
        checkout_dir: checkout,
        spec_path: spec_path.clone(),
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
        carried: None,
    };
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let result = fix_after::fix_after_build(&ctx, &mut st, &step, &build_log, None);
    (result, std::fs::read_to_string(spec_path).unwrap())
}

#[test]
fn unowned_directories_are_fixed_without_an_agent() {
    let (result, spec) = run(UNOWNED);
    assert!(matches!(result, Ok(RoundEnd::Retry)));
    assert!(spec.contains("%dir %{_datadir}/jay\n%{_datadir}/jay/jay.conf"));
}

#[test]
fn another_failure_ends_the_run_and_leaves_the_spec() {
    let (result, spec) = run("[  9s] error: Bad exit status from /var/tmp/rpm-tmp (%build)\n");
    match result {
        Err(PipelineResult::Failed { reason, .. }) => {
            assert!(reason.starts_with("build failed; log:"), "{reason}")
        }
        _ => panic!("the run did not fail"),
    }
    assert_eq!(spec, SPEC);
}
