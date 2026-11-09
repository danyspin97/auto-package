//! A refused wheel switch (it would drop a security patch) at the callers that
//! are not the ladder: the kind each one ends the package with.

use super::*;
use crate::pipeline::patch_condition::patch_condition_gate;
use crate::pipeline::removal_proof::wheel_drop_problem;

const PATCH: &str = "fix-CVE-2024-1234.patch";
const WHEEL_JSON: &str = r#"{"info":{"name":"foo","version":"1.0"},"urls":[
  {"digests":{"md5":"aaa","sha256":"0123456789abcdef"},"filename":"foo-1.0-py3-none-any.whl",
   "packagetype":"bdist_wheel","size":10,"url":"https://files.pythonhosted.org/foo-1.0-py3-none-any.whl",
   "yanked":false}]}"#;

/// The spec of `python-foo`: it declares a security patch and ships `/usr/bin/foo`.
fn spec() -> String {
    format!(
        "Name: python-foo\nVersion: 1.0\nRelease: 0\nSummary: x\nLicense: MIT\nPatch0: {PATCH}\n\
         %description\nx\n%files\n/usr/bin/foo\n"
    )
}

/// Runs `f` on a run context and state for `python-foo` with a checkout whose
/// spec declares a security patch.
fn with_ctx<R>(f: impl FnOnce(&RunCtx, &mut RunState, &CheckoutSnapshot, &StepDir) -> R) -> R {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("python-foo", &state_root);
    config.rpm_name = "python-foo".into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.no_agent = false;
    config.budget_usd = 100.0;
    let checkout = work.join("proj").join("python-foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let spec_path = checkout.join("python-foo.spec");
    std::fs::write(&spec_path, spec()).unwrap();
    std::fs::write(checkout.join(PATCH), "--- a/f\n+++ b/f\n").unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("python-foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws: Workspace::open(&state_root, "python-foo").unwrap(),
        rpm_name: "python-foo".into(),
        obs_name: "python-foo".into(),
        checkout_dir: checkout.clone(),
        spec_path,
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
    let snapshot = CheckoutSnapshot::take(&checkout);
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    f(&ctx, &mut st, &snapshot, &step)
}

fn lookup(_: &PipelineConfig) -> Result<String, String> {
    Ok(WHEEL_JSON.to_string())
}

fn kind_of(result: Result<impl std::fmt::Debug, PipelineResult>) -> FailureKind {
    match result {
        Err(PipelineResult::Failed { kind, .. }) => kind,
        other => panic!("{other:?}"),
    }
}

#[test]
fn resolve_blocked_ends_the_package_on_a_refusal_and_a_later_failure_is_its_own() {
    with_ctx(|ctx, st, snapshot, step| {
        let before = spec();
        let site = || LadderSite {
            build_log: None,
            task: crate::agent::Task::Review,
            event_task: ap_core::event::Task::Review,
            attempt: 0,
            hint: None,
            context: None,
            apply_label: "Review",
            before: &before,
            snapshot,
            step,
            staged: &[],
        };
        let outcome = pipeline_wheel_outcome("a wheel instead, for a reason long enough");
        let refusal = TurnError::needs_maintainer(wheel_drop_problem(&[PATCH.to_string()]));
        // The turn is not a `blocked` to resolve: no ladder round runs (a turn
        // here would be a real agent call).
        let result = resolve_blocked(ctx, st, site(), &outcome, refusal);
        assert_eq!(kind_of(result), FailureKind::NeedsMaintainer);
        // The same spec path fails later for another reason: an ordinary failure.
        let later = resolve_blocked(
            ctx,
            st,
            site(),
            &outcome,
            "the edit was rejected".to_string().into(),
        );
        assert_eq!(kind_of(later), FailureKind::Other);
        assert_eq!(
            kind_of(Err::<(), _>(ctx.failed("a later failure".into()))),
            FailureKind::Other
        );
    });
}

#[test]
fn the_patch_condition_gate_ends_a_refused_switch_as_needs_maintainer() {
    with_ctx(|ctx, st, snapshot, step| {
        let cond = crate::ship_condition::FilesCondition {
            path: "usr/lib/foo".into(),
            package: None,
            not_in: Vec::new(),
        };
        let before = spec();
        let gate = |st: &mut RunState| {
            patch_condition_gate(
                ctx,
                st,
                ApplyTurnCtx {
                    config: ctx.config,
                    checkout_dir: &ctx.checkout_dir,
                    spec_path: &ctx.spec_path,
                    step,
                    before: &before,
                    rpm_name: "python-foo",
                    label: "Patch",
                    attempt: 1,
                    notes_path: ctx.ws.notes(),
                    snapshot: Some(snapshot),
                    kept: &[],
                    staged_out: None,
                    rejected_out: None,
                },
                &cond,
                &lookup,
            )
        };
        // The first failed patch only queues the request again; the second
        // switches to the wheel, which is refused.
        assert_eq!(kind_of(gate(st)), FailureKind::ConditionUnmet);
        let result = gate(st);
        let PipelineResult::Failed { reason, kind, .. } = result.unwrap_err() else {
            panic!("not a failure");
        };
        assert_eq!(kind, FailureKind::NeedsMaintainer, "{reason}");
        assert!(reason.contains(PATCH), "{reason}");
    });
}

#[test]
fn the_submodule_wheel_switch_ends_a_refused_switch_as_needs_maintainer() {
    with_ctx(|ctx, st, _snapshot, _step| {
        let root = ctx.ws.sdist_dir().join(&st.sdist_top_level);
        for d in ["a", "b"] {
            std::fs::create_dir_all(root.join("third_party").join(d)).unwrap();
        }
        st.from_repo_archive = true;
        let result = switch_for_missing_submodules(ctx, st, &ctx.spec_path, &lookup);
        assert_eq!(kind_of(result), FailureKind::NeedsMaintainer);
        // Nothing of the switch is left in the checkout.
        let names: Vec<String> = std::fs::read_dir(&ctx.checkout_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".whl")), "{names:?}");
        assert_eq!(std::fs::read_to_string(&ctx.spec_path).unwrap(), spec());
    });
}

#[test]
fn a_rerun_of_a_ladder_refused_its_wheel_ends_as_the_first_run_did() {
    with_ctx(|ctx, st, snapshot, step| {
        let before = spec();
        let site = LadderSite {
            build_log: None,
            task: crate::agent::Task::Review,
            event_task: ap_core::event::Task::Review,
            attempt: 0,
            hint: None,
            context: None,
            apply_label: "Review",
            before: &before,
            snapshot,
            step,
            staged: &[],
        };
        let blocked = |why: &str| {
            let mut out = pipeline_wheel_outcome("x");
            out.action = crate::agent::Action::Blocked { why: why.into() };
            out.reasoning = why.into();
            out
        };
        let first = blocked("zero");
        let first_result = resolve_blocked_with(
            ctx,
            st,
            site,
            &first,
            "first turn blocked".into(),
            &lookup,
            &mut |_, _, site, _, _, _| {
                Ok((
                    StepDir {
                        path: site.step.path.clone(),
                        id: ap_core::event::StepId(2),
                    },
                    blocked("again"),
                ))
            },
        );
        let (kind, reason) = match first_result {
            Err(PipelineResult::Failed { kind, reason, .. }) => (kind, reason),
            other => panic!("{other:?}"),
        };
        assert_eq!(kind, FailureKind::NeedsMaintainer, "{reason}");
        // The same spec again, without --force: the memo ends it the same way.
        let rerun = spent_ladder_refusal(ctx, false, &before).expect("the ladder was spent");
        let PipelineResult::Failed {
            kind: again,
            reason: again_reason,
            ..
        } = rerun
        else {
            panic!("not a failure");
        };
        assert_eq!(again, FailureKind::NeedsMaintainer);
        assert!(again_reason.contains(PATCH), "{again_reason}");
        assert!(again_reason.contains("not trying again"), "{again_reason}");
    });
}
