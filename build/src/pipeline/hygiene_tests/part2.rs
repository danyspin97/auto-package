use super::*;
use crate::pipeline::patch_condition::*;

#[test]
fn a_failed_state_that_obs_built_is_reconciled_by_downloading_its_rpms() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    // A project name with `:` in it, as the checkout root has.
    let checkouts = root.path().join("home:u:p");
    let package = |name: &str, outcome: &str, built: &str, spec: &str| {
        write_state(&state_root, name, outcome, Some("OBS: build failed"));
        let ws = Workspace::open(&state_root, name).unwrap();
        PkgState::update(&ws, |s| s.last_built_version = Some(built.into())).unwrap();
        std::fs::create_dir_all(checkouts.join(name)).unwrap();
        std::fs::write(
            checkouts.join(name).join(format!("{name}.spec")),
            format!("Name: {name}\nVersion: {spec}\n"),
        )
        .unwrap();
    };
    package("python-ok", "failed", "1.0", "1.0");
    package("python-old", "failed", "1.0", "2.0");
    package("python-dirty", "failed", "1.0", "1.0");
    package("python-empty", "interrupted", "1.0", "1.0");
    package("python-built", "success", "1.0", "1.0");
    package("python-bad", "failed", "1.0", "1.0");
    let statuses: std::collections::HashMap<String, osc::BuildStatus> = [
        "python-ok",
        "python-old",
        "python-dirty",
        "python-empty",
        "python-built",
    ]
    .iter()
    .map(|n| (n.to_string(), osc::BuildStatus::Succeeded))
    .chain([("python-bad".to_string(), osc::BuildStatus::Failed)])
    .collect();
    let downloads = std::sync::Mutex::new(Vec::new());
    let rpms_dir = root.path().join("rpms");
    let done = reconcile_with_obs(
        &state_root,
        &checkouts,
        &rpms_dir,
        &statuses,
        |dir| dir.ends_with("python-dirty"),
        |name| {
            downloads.lock().unwrap().push(name.to_string());
            // A download leaves OBS's rpmlint log in the stash.
            let stash = rpms_dir.join(".rpmlint");
            std::fs::create_dir_all(&stash).unwrap();
            std::fs::write(stash.join(format!("{name}.log")), "x.rpm: E: no-binary\n").unwrap();
            Ok(if name == "python-empty" { 0 } else { 3 })
        },
    );
    // Only the package OBS built, at the version the state worked on, with a
    // clean checkout, is fetched; an empty download changes nothing.
    assert_eq!(*downloads.lock().unwrap(), ["python-empty", "python-ok"]);
    assert_eq!(
        outcome(&state_root, "python-ok").as_deref(),
        Some("success")
    );
    let ws = Workspace::open(&state_root, "python-ok").unwrap();
    assert_eq!(PkgState::load(&ws).last_failure_reason, None);
    for kept in ["python-old", "python-dirty", "python-bad"] {
        assert_eq!(
            outcome(&state_root, kept).as_deref(),
            Some("failed"),
            "{kept}"
        );
    }
    assert_eq!(
        outcome(&state_root, "python-empty").as_deref(),
        Some("interrupted")
    );
    assert_eq!(done.len(), 2, "{done:?}");
    // The rpmlint log of the fetched RPMs is recorded at once.
    let log = std::fs::read_to_string(ws.dir().join("rpmlint.log")).unwrap();
    assert!(
        log.contains("rpmlint of python-ok 1.0") && log.contains("E: no-binary"),
        "{log}"
    );
    assert!(!rpms_dir.join(".rpmlint").join("python-ok.log").exists());
    // A failing download is reported, not fatal.
    let again = reconcile_with_obs(
        &state_root,
        &checkouts,
        &rpms_dir,
        &statuses,
        |_| false,
        |_| Err(osc::Error::NotActionable("502".into())),
    );
    assert!(
        again
            .iter()
            .all(|(_, m)| matches!(m, Err(e) if e.contains("fetching its RPMs failed"))),
        "{again:?}"
    );
}

pub(super) struct Warnings(pub(super) std::sync::Mutex<Vec<String>>);

impl ap_core::sink::Reporter for Warnings {
    fn report(&self, event: &Event) {
        if let Event::Warning { message, .. } = event {
            self.0.lock().unwrap().push(message.clone());
        }
    }
}

#[test]
fn a_stale_failure_is_reconciled_only_when_the_rpms_download() {
    let root = tempfile::tempdir().unwrap();
    write_state(root.path(), "python-x", "failed", Some("old"));
    let ws = Workspace::open(root.path(), "python-x").unwrap();
    let cfg = config("x", root.path());
    let quiet = Warnings(Default::default());
    assert!(reconcile_rpms_with(&cfg, "python-x", &ws, &quiet, || Ok(2)));
    assert!(quiet.0.lock().unwrap().is_empty());
    // Nothing landed: the package is rebuilt, with a warning saying why.
    assert!(!reconcile_rpms_with(&cfg, "python-x", &ws, &quiet, || Ok(
        0
    )));
    assert!(quiet.0.lock().unwrap()[0].contains("no RPMs were downloaded"));
    // A failing download is the same.
    assert!(!reconcile_rpms_with(&cfg, "python-x", &ws, &quiet, || {
        Err(osc::Error::NotActionable("502".into()))
    }));
    assert!(quiet.0.lock().unwrap()[1].contains("fetching its RPMs failed"));
    // A recorded success needs no download at all.
    write_state(root.path(), "python-x", "success", None);
    assert!(reconcile_rpms_with(
        &cfg,
        "python-x",
        &ws,
        &quiet,
        || panic!("not called")
    ));
}

pub(crate) fn config(pkg: &str, state_root: &Path) -> PipelineConfig {
    PipelineConfig {
        patch_provenance: false,
        pkg_name: pkg.into(),
        rpm_name: osc::rpm_name(pkg),
        version: "1.0".into(),
        project: "p".into(),
        work_dir: PathBuf::new(),
        state_root: state_root.to_path_buf(),
        rpms_dir: PathBuf::new(),
        distro: "openSUSE_Tumbleweed".into(),
        arch: "x86_64".into(),
        max_attempts: 1,
        budget_usd: 1.0,
        no_agent: true,
        force: false,
        regen: false,
        update: false,
        branch_version: None,
        source_repo: None,
        upstream_tag: None,
        subprojects: Vec::new(),
        failed_deps: Vec::new(),
        unknown_build_system: false,
        unbuildable_deps: Vec::new(),
        run_id: "test".into(),
        source_urls: Default::default(),
        python_flavors: Default::default(),
        patch_request: None,
        patch_condition: None,
        bug_fix: None,
    }
}

#[test]
fn wheel_switch_provenance_goes_to_the_state_dir_not_the_checkout() {
    let state = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let config = config("foo", state.path());
    let wheel = ap_ecosystem::pypi::WheelFile {
        filename: "foo-1.0-py3-none-any.whl".into(),
        url: "https://h/foo.whl".into(),
        sha256: "ab".into(),
        size: 3,
    };
    write_wheel_switch_provenance("python-foo", &config, &wheel, 1, "build", "why", "SPEC")
        .unwrap();
    let dir = state.path().join("python-foo");
    assert!(dir.join("wheel-origin.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.join("python-foo.spec.pre-wheel-switch")).unwrap(),
        "SPEC"
    );
    assert_eq!(std::fs::read_dir(checkout.path()).unwrap().count(), 0);
}

/// A release whose one wheel is `bytes`, as PyPI's JSON describes it.
pub(super) fn pypi_json_for(wheel: &str, bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let sha: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        r#"{{"info":{{"name":"foo","version":"1.0","summary":"Foo","license":"MIT",
            "requires_python":">=3.9"}},
            "urls":[{{"filename":"{wheel}","url":"https://h/{wheel}","packagetype":"bdist_wheel",
            "size":{},"digests":{{"sha256":"{sha}"}}}}]}}"#,
        bytes.len()
    )
}

#[test]
fn a_missing_submodule_tree_switches_to_the_wheel_end_to_end_without_the_network() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkout = root.path().join("proj").join("python-foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let config = config("foo", &state_root);
    let ws = Workspace::open(&state_root, "python-foo").unwrap();
    // The unpacked source archive: a submodule directory with nothing in it.
    for d in ["pybind11", "XNNPACK"] {
        std::fs::create_dir_all(ws.sdist_dir().join("foo-1.0/third_party").join(d)).unwrap();
    }
    std::fs::write(ws.sdist_dir().join("foo-1.0/setup.py"), "x").unwrap();
    let spec_path = checkout.join("python-foo.spec");
    std::fs::write(
        &spec_path,
        "Name: python-foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n",
    )
    .unwrap();
    // The wheel is already in the checkout with the digest PyPI reports, so
    // nothing is downloaded.
    let wheel = "foo-1.0-py3-none-any.whl";
    let bytes = b"not a real wheel";
    std::fs::write(checkout.join(wheel), bytes).unwrap();
    let json = pypi_json_for(wheel, bytes);
    let asked = std::sync::Mutex::new(Vec::new());
    let lookup = |c: &PipelineConfig| {
        asked
            .lock()
            .unwrap()
            .push(format!("{} {}", c.pkg_name, c.version));
        Ok(json.clone())
    };

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
        checkout_dir: checkout.clone(),
        spec_path: spec_path.clone(),
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: "foo-1.0".into(),
        from_repo_archive: true,
        attempts: 3,
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
    };

    let switched = switch_for_missing_submodules(&ctx, &mut st, &spec_path, &lookup)
        .unwrap_or_else(|_| panic!("the switch step failed"));

    // The Review turn is skipped, and the lookup ran once for this release.
    assert!(switched, "{:?}", warnings.0.lock().unwrap());
    assert_eq!(*asked.lock().unwrap(), ["foo 1.0"]);
    // The recipe is now the wheel's, and the source-tree state is reset.
    let spec = std::fs::read_to_string(&spec_path).unwrap();
    assert!(spec.contains(wheel), "{spec}");
    assert_eq!(
        (st.sdist_top_level.as_str(), st.from_repo_archive),
        ("", false)
    );
    // Provenance sits in the state directory, with the spec that was replaced.
    let dir = state_root.join("python-foo");
    let origin = std::fs::read_to_string(dir.join("wheel-origin.txt")).unwrap();
    assert!(
        origin.contains(wheel) && origin.contains("third_party/pybind11"),
        "{origin}"
    );
    assert!(
        std::fs::read_to_string(dir.join("python-foo.spec.pre-wheel-switch"))
            .unwrap()
            .contains("Source0: foo-1.0.tar.gz")
    );
    assert!(!checkout.join("wheel-origin.txt").exists());
    // The notes say what was swapped.
    let notes = std::fs::read_to_string(ctx.ws.notes()).unwrap();
    assert!(
        notes.contains("switched to upstream wheel") && notes.contains(wheel),
        "{notes}"
    );
}

// ── conditions on patch requests (the torch `-devel` split) ─────────────

pub(super) const SPLIT_SPEC: &str = "Name: python-foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n\n%files %{python_files}\n%{python_sitearch}/foo\n%exclude %{python_sitearch}/foo/utils\n\n%files %{python_files devel}\n%{python_sitearch}/foo/utils\n";
pub(super) const FIXED_SPEC: &str = "Name: python-foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n\n%files %{python_files}\n%{python_sitearch}/foo\n\n%files %{python_files devel}\n%{python_sitearch}/foo/include\n";

pub(super) fn utils_condition() -> crate::ship_condition::FilesCondition {
    crate::ship_condition::FilesCondition {
        path: "foo/utils".into(),
        package: Some("python-foo".into()),
        not_in: Vec::new(),
    }
}

pub(super) fn request(
    problem: &str,
    condition: Option<crate::ship_condition::FilesCondition>,
) -> crate::agent::PatchRequest {
    crate::agent::PatchRequest {
        name: "foo".into(),
        problem: problem.into(),
        condition,
    }
}

#[test]
fn a_conditioned_request_is_over_when_the_condition_holds_not_when_the_cap_is_hit() {
    let never = |_: &crate::ship_condition::FilesCondition| false;
    let always = |_: &crate::ship_condition::FilesCondition| true;
    let cond = utils_condition();
    let key = condition_key(&cond);
    // The loop: two patches already made, the old cap calls any request over.
    let mut state = PkgState {
        patched_problems: vec![1, 2],
        ..Default::default()
    };
    assert!(patch_request_over(
        &state,
        &request("reworded", None),
        &never
    ));
    // With a condition, the cap does not settle it: the condition decides.
    assert!(!patch_request_over(
        &state,
        &request("reworded", Some(cond.clone())),
        &never
    ));
    assert!(patch_request_over(
        &state,
        &request("reworded", Some(cond.clone())),
        &always
    ));
    // Found true before: over without a check.
    state.record_condition_satisfied(key);
    assert!(patch_request_over(
        &state,
        &request("again, worded another way", Some(cond.clone())),
        &never
    ));
    // Failures are counted, and the request is given up on only when spent.
    let mut state = PkgState::default();
    assert_eq!(state.record_condition_failure(key), 1);
    assert_eq!(state.record_condition_failure(key), 2);
    assert!(!patch_request_over(
        &state,
        &request("x", Some(cond.clone())),
        &never
    ));
    assert_eq!(state.record_condition_failure(key), 3);
    assert!(patch_request_over(
        &state,
        &request("x", Some(cond.clone())),
        &never
    ));
    // An invalid condition is ignored, falling back to the text and the cap.
    let bad = crate::ship_condition::FilesCondition {
        path: "../x".into(),
        package: None,
        not_in: vec![],
    };
    assert!(!patch_request_over(
        &PkgState::default(),
        &request("x", Some(bad)),
        &never
    ));
}

#[test]
fn a_condition_that_is_true_of_the_recipe_already_is_found_without_a_turn() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config("foo", &root.path().join("state"));
    config.work_dir = root.path().join("work");
    config.project = "proj".into();
    config.rpms_dir = root.path().join("no-rpms");
    let dir = config.work_dir.join("proj/python-foo");
    std::fs::create_dir_all(&dir).unwrap();
    let cond = utils_condition();
    std::fs::write(dir.join("python-foo.spec"), SPLIT_SPEC).unwrap();
    assert!(!condition_holds_now(
        &config,
        "python-foo",
        "python-foo",
        &cond
    ));
    std::fs::write(
        dir.join("python-foo.spec"),
        FIXED_SPEC.replace("foo/include", "foo/utils"),
    )
    .unwrap();
    // Still devel: not true.
    assert!(!condition_holds_now(
        &config,
        "python-foo",
        "python-foo",
        &cond
    ));
    std::fs::write(dir.join("python-foo.spec"), FIXED_SPEC).unwrap();
    assert!(condition_holds_now(
        &config,
        "python-foo",
        "python-foo",
        &cond
    ));
}

/// A package `python-foo` whose spec is `spec`, with the wheel the switch
/// would take already in its checkout.
pub(super) fn gate_case(
    spec: &str,
    failures_before: u32,
    lookup_ok: bool,
) -> (
    Result<(), PipelineResult>,
    PkgState,
    String,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkout = root.path().join("proj").join("python-foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let config = config("foo", &state_root);
    let ws = Workspace::open(&state_root, "python-foo").unwrap();
    std::fs::create_dir_all(ws.sdist_dir().join("foo-1.0")).unwrap();
    std::fs::write(ws.sdist_dir().join("foo-1.0/setup.py"), "x").unwrap();
    let spec_path = checkout.join("python-foo.spec");
    std::fs::write(&spec_path, spec).unwrap();
    let wheel = "foo-1.0-py3-none-any.whl";
    let bytes = b"not a real wheel";
    std::fs::write(checkout.join(wheel), bytes).unwrap();
    let json = pypi_json_for(wheel, bytes);
    let lookup = |_: &PipelineConfig| {
        if lookup_ok {
            Ok(json.clone())
        } else {
            Err("no PyPI".to_string())
        }
    };
    let cond = utils_condition();
    let key = condition_key(&cond);
    for _ in 0..failures_before {
        update_state(&ws, &ap_core::sink::NULL_REPORTER, |s| {
            s.record_condition_failure(key);
        });
    }
    let run = ap_core::run::RunDir::allocate(&root.path().join("work")).unwrap();
    let pkg_run = run.pkg("python-foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: "python-foo".into(),
        obs_name: "python-foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: spec_path.clone(),
        signals: Default::default(),
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
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
    };
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let now = std::fs::read_to_string(&spec_path).unwrap();
    let result = patch_condition_gate(
        &ctx,
        &mut st,
        ApplyTurnCtx {
            config: &config,
            checkout_dir: &checkout,
            spec_path: &spec_path,
            step: &step,
            before: &now,
            rpm_name: "python-foo",
            label: "PatchDependency - upstream wheel",
            attempt: 0,
            notes_path: ctx.ws.notes(),
            snapshot: None,
            kept: &[],
            staged_out: None,
            rejected_out: None,
        },
        &cond,
        &lookup,
    );
    let state = PkgState::load(&ctx.ws);
    let spec_after = std::fs::read_to_string(&spec_path).unwrap();
    (result, state, spec_after, root)
}

#[test]
fn a_patch_that_meets_the_condition_marks_the_request_satisfied() {
    let (result, state, spec, _root) = gate_case(FIXED_SPEC, 0, false);
    assert!(result.is_ok());
    assert!(
        state
            .satisfied_conditions
            .contains(&condition_key(&utils_condition()))
    );
    assert_eq!(spec, FIXED_SPEC);
}
