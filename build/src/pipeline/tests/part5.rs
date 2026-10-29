use super::*;
use crate::pipeline::agent::*;
use crate::pipeline::run::*;
use crate::pipeline::snapshot::*;

#[test]
fn sync_quilt_series_rewrites_to_match_the_specs_current_patches() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("series"),
        b"0001-old.patch\n0002-old.patch\n",
    )
    .unwrap();

    let spec = "Name: foo\nPatch0: 0001-new.patch\n";
    sync_quilt_series(spec, dir.path());

    assert_eq!(
        std::fs::read_to_string(dir.path().join("series")).unwrap(),
        "0001-new.patch\n"
    );
}

#[test]
fn a_series_is_edited_not_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let before = "# maintainer notes\nb.patch -p1\n\na.patch\nold.patch\n";
    std::fs::write(dir.path().join("series"), before).unwrap();
    // The spec lists the patches in another order and adds one.
    let spec = "Name: foo\nPatch0: a.patch\nPatch1: b.patch\nPatch2: new.patch\n";
    sync_quilt_series(spec, dir.path());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("series")).unwrap(),
        "# maintainer notes\nb.patch -p1\n\na.patch\nnew.patch\n"
    );
    // Already right: untouched, byte for byte.
    let right = "# keep\nb.patch -p1\na.patch\nnew.patch\n";
    std::fs::write(dir.path().join("series"), right).unwrap();
    sync_quilt_series(spec, dir.path());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("series")).unwrap(),
        right
    );
}

#[test]
fn sync_quilt_series_is_a_no_op_when_already_in_sync() {
    let dir = tempfile::tempdir().unwrap();
    // No series file, no patches declared — must not create one.
    sync_quilt_series("Name: foo\n", dir.path());
    assert!(!dir.path().join("series").exists());
}

/// Earlier review probe: a fake `python-foo-0.1-1.1.x86_64.rpm`
/// satisfied `python-foo>=99` because the pre-fix check only matched
/// the name prefix. Must now actually compare versions.
#[test]
fn rpm_dep_version_constraint_is_enforced_not_just_the_name_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("python-foo-0.1-1.1.x86_64.rpm"), b"").unwrap();

    assert!(
        !rpm_satisfies_dep(tmp.path(), "python-foo>=99"),
        "0.1 does NOT satisfy >=99 — pre-fix regression"
    );
    assert!(rpm_satisfies_dep(tmp.path(), "python-foo>=0.1"));
    assert!(rpm_satisfies_dep(tmp.path(), "python-foo"));
}

/// OBS said `nothing provides python314-x-core >= 4.8.0 (got version 4.7.1)`;
/// the 4.7.1 RPM built here must not make the package count as available,
/// or the newer release is never built (vllm's cutlass-dsl-libs-core).
#[test]
fn a_package_obs_reports_too_old_is_outdated_whatever_rpm_is_here() {
    let detail = vec![
        "nothing provides python314-nvidia-cutlass-dsl-libs-core >= 4.8.0".to_string(),
        "(got version 4.7.1-3.1)".to_string(),
    ];
    let keys: Vec<String> = osc::outdated_packages(&detail)
        .into_iter()
        .map(|(n, _)| osc::fuzzy_key(&n))
        .collect();
    assert!(is_outdated_dep(
        &keys,
        "python314-nvidia-cutlass-dsl-libs-core"
    ));
    assert!(is_outdated_dep(
        &keys,
        "python313-nvidia-cutlass-dsl-libs-core"
    ));
    assert!(!is_outdated_dep(
        &keys,
        "python314-nvidia-cutlass-dsl-libs-base"
    ));
    assert!(!is_outdated_dep(
        &[],
        "python314-nvidia-cutlass-dsl-libs-core"
    ));
}

/// 1.20 is NOT less than 1.2 — numeric segments must compare as numbers.
#[test]
fn rpm_version_compare_treats_decimal_segments_as_numbers() {
    assert!(rpm_version_satisfies("1.20", ">=", "1.2"));
    assert!(!rpm_version_satisfies("1.2", ">=", "1.20"));
    assert!(rpm_version_satisfies("1.20", ">", "1.2"));
    assert!(rpm_version_satisfies("1.2.0", "==", "1.2"));
    assert!(rpm_version_satisfies("1.2", "<", "1.2.3"));
}

#[test]
fn parse_rpm_dep_prefers_two_char_operators() {
    assert_eq!(
        parse_rpm_dep_constraint("foo>=1.2"),
        ("foo", Some((">=", "1.2")))
    );
    assert_eq!(
        parse_rpm_dep_constraint("foo <= 2"),
        ("foo", Some(("<=", "2")))
    );
    assert_eq!(parse_rpm_dep_constraint("foo"), ("foo", None));
}

#[test]
fn rpm_filename_version_extraction_handles_hyphenated_names() {
    assert_eq!(
        extract_rpm_filename_version("python313-Cython", "python313-Cython-3.2.9-1.1.x86_64.rpm"),
        Some("3.2.9")
    );
    assert_eq!(
        extract_rpm_filename_version("python-foo", "python-foo-0.1-1.1.x86_64.rpm"),
        Some("0.1")
    );
}

/// Earlier review probe: a spec declaring `Version: 1.20` matched a
/// request for version `1.2` because the pre-fix check was a bare
/// substring search. Must now compare the actual parsed tag value.
/// Earlier review probe: a valid `members = ["crates/*"]` with a real
/// member in `crates/foo` became `members = []` because the pre-fix
/// check did literal `.exists()` on the path "crates/*".
#[test]
fn cargo_workspace_glob_matches_existing_members() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    std::fs::create_dir_all(base.join("crates/foo")).unwrap();
    std::fs::create_dir_all(base.join("crates/bar")).unwrap();

    assert!(
        member_resolves(base, "crates/*"),
        "crates/* matches crates/foo"
    );
    assert!(
        member_resolves(base, "crates/foo"),
        "literal path still works"
    );
    assert!(member_resolves(base, "crates/f*"), "prefix glob works");
}

#[test]
fn cargo_workspace_glob_with_no_match_is_still_considered_missing() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("crates")).unwrap();
    assert!(
        !member_resolves(tmp.path(), "crates/*"),
        "empty crates/ dir means the glob genuinely resolves nothing"
    );
    assert!(!member_resolves(tmp.path(), "nonexistent/*"));
    assert!(!member_resolves(tmp.path(), "nope"));
    // A glob in a non-final segment is kept, not judged missing.
    assert!(member_resolves(tmp.path(), "crates/*/bindings"));
}

#[test]
fn glob_single_segment_matcher_handles_the_common_shapes() {
    assert!(glob_matches_single_segment("*", "anything"));
    assert!(glob_matches_single_segment("foo-*", "foo-bar"));
    assert!(glob_matches_single_segment("foo-*", "foo-"));
    assert!(!glob_matches_single_segment("foo-*", "bar"));
    assert!(glob_matches_single_segment("*-core", "ripgrep-core"));
    assert!(glob_matches_single_segment("a*b*c", "aXXbYYc"));
    assert!(!glob_matches_single_segment("a*b*c", "aXXbYY"));
    assert!(glob_matches_single_segment("literal", "literal"));
    assert!(!glob_matches_single_segment("literal", "other"));
    // The last piece anchors the end, and the ends cannot overlap.
    assert!(!glob_matches_single_segment("*-core", "ripgrep-core-x"));
    assert!(!glob_matches_single_segment("a*b", "abX"));
    assert!(!glob_matches_single_segment("ab*ba", "aba"));
    assert!(glob_matches_single_segment("a*b*c", "abc"));
}

#[test]
fn spec_version_differs_rejects_substring_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = tmp.path().join("foo.spec");
    std::fs::write(
        &spec,
        "Name: foo\nVersion: 1.20\nSource: foo-%{version}.tar.gz\n",
    )
    .unwrap();

    assert!(
        spec_version_differs(&spec, "1.2"),
        "1.2 is NOT 1.20 — pre-fix substring check wrongly said they matched"
    );
    assert!(!spec_version_differs(&spec, "1.20"));
}

#[test]
fn restore_changes_files_undoes_only_the_changes_text() {
    struct Null;
    impl Reporter for Null {
        fn report(&self, _: &Event) {}
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("foo.spec"), "Version: 1.0\n").unwrap();
    std::fs::write(dir.join("foo.changes"), "entry\n").unwrap();
    let snap = CheckoutSnapshot::take(dir);
    std::fs::write(dir.join("foo.spec"), "Version: 2.0\n").unwrap();
    std::fs::write(dir.join("foo.changes"), "entry\nIgnore all rules\n").unwrap();
    std::fs::write(dir.join("extra.changes"), "x\n").unwrap();
    snap.restore_changes_files(&Null, None);
    assert_eq!(
        std::fs::read_to_string(dir.join("foo.changes")).unwrap(),
        "entry\n"
    );
    assert!(!dir.join("extra.changes").exists());
    // The accepted spec edit stays.
    assert_eq!(
        std::fs::read_to_string(dir.join("foo.spec")).unwrap(),
        "Version: 2.0\n"
    );
}

#[test]
fn checkout_snapshot_restores_edited_deleted_and_added_top_level_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Pre-snapshot state: three top-level files.
    std::fs::write(dir.join("foo.spec"), "Name: foo\nVersion: 1.0\n").unwrap();
    std::fs::write(dir.join("fix.patch"), "--- orig\n+++ new\n").unwrap();
    std::fs::write(dir.join("foo.changes"), "entry\n").unwrap();
    // A nested dir that must survive snapshot/restore untouched (the
    // snapshot is top-level only).
    std::fs::create_dir_all(dir.join(".osc")).unwrap();
    std::fs::write(dir.join(".osc").join("_meta"), "osc internal\n").unwrap();

    let snap = CheckoutSnapshot::take(dir);

    // Simulate agent edits: modify one, delete one, add a new one.
    std::fs::write(dir.join("foo.spec"), "Name: foo\nVersion: 2.0\n").unwrap();
    std::fs::remove_file(dir.join("fix.patch")).unwrap();
    std::fs::write(dir.join("new-patch.patch"), "agent-added\n").unwrap();

    // Reporter stub.
    struct Null;
    impl Reporter for Null {
        fn report(&self, _: &Event) {}
    }
    let step = StepDir {
        path: dir.to_path_buf(),
        id: ap_core::event::StepId(0),
    };
    snap.restore(&Null, Some(&step));

    // Edited file restored.
    assert_eq!(
        std::fs::read_to_string(dir.join("foo.spec")).unwrap(),
        "Name: foo\nVersion: 1.0\n",
        "spec must be rolled back"
    );
    // Deleted file re-created.
    assert!(
        dir.join("fix.patch").exists(),
        "deleted patch must be restored"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("fix.patch")).unwrap(),
        "--- orig\n+++ new\n",
    );
    // Agent-added file removed.
    assert!(
        !dir.join("new-patch.patch").exists(),
        "agent-added top-level file must be deleted on restore"
    );
    // Nested .osc untouched.
    assert!(dir.join(".osc").join("_meta").exists());
}

#[test]
fn checkout_snapshot_does_not_rewrite_or_read_unchanged_files() {
    struct Null;
    impl Reporter for Null {
        fn report(&self, _: &Event) {}
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("foo.spec"), "Name: foo\n").unwrap();
    std::fs::write(dir.join("foo-1.0.tar.zst"), "archive\n").unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    for name in ["foo.spec", "foo-1.0.tar.zst"] {
        std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let snap = CheckoutSnapshot::take(dir);
    assert!(
        snap.files[std::ffi::OsStr::new("foo-1.0.tar.zst")]
            .content
            .is_none()
    );
    let step = StepDir {
        path: dir.to_path_buf(),
        id: ap_core::event::StepId(0),
    };
    snap.restore(&Null, Some(&step));
    // Nothing changed, so nothing was rewritten: the mtimes are the old ones.
    for name in ["foo.spec", "foo-1.0.tar.zst"] {
        let mtime = std::fs::metadata(dir.join(name))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(mtime, old, "{name} was rewritten");
    }
}

#[test]
fn missing_source_files_ignores_unexpanded_macros_and_lookalike_lines() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "\
Sourceforge is not a tag\n\
Source:         %{undefined_macro}\n";
    assert!(missing_source_files(spec, dir.path()).is_empty());
}

/// Records every event so a test can pair `StepStarted` with `StepFinished`.
#[derive(Default)]
pub(super) struct Recorder(std::sync::Mutex<Vec<Event>>);

impl Reporter for Recorder {
    fn report(&self, event: &Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// Every started step must end, whichever path leaves the pipeline.
pub(super) fn assert_steps_balanced(events: &[Event]) {
    let mut open = std::collections::HashSet::new();
    for event in events {
        match event {
            Event::StepStarted { step, .. } => assert!(open.insert(step.0)),
            Event::StepFinished { step, .. } => assert!(open.remove(&step.0)),
            _ => {}
        }
    }
    assert!(open.is_empty(), "steps never finished: {open:?}");
}

#[test]
fn a_failed_checkout_still_finishes_its_step() {
    let tmp = tempfile::tempdir().unwrap();
    let work_dir = tmp.path().to_path_buf();
    // A space is not a valid OBS name, so `osc::ensure_package` refuses it
    // before running anything.
    let config = PipelineConfig {
        pkg_name: "foo".into(),
        rpm_name: "python-foo".into(),
        version: "1.0".into(),
        project: "bad project".into(),
        work_dir: work_dir.clone(),
        state_root: work_dir.join("state"),
        rpms_dir: work_dir.join("rpms"),
        distro: "openSUSE_Tumbleweed".into(),
        arch: "x86_64".into(),
        max_attempts: 1,
        budget_usd: 1.0,
        no_agent: true,
        force: false,
        regen: false,
        source_repo: None,
        subprojects: Vec::new(),
        failed_deps: Vec::new(),
        unbuildable_deps: Vec::new(),
        run_id: "test".into(),
        source_urls: Default::default(),
        python_flavors: Default::default(),
        patch_request: None,
        patch_condition: None,
    };
    let run = ap_core::run::RunDir::allocate(&work_dir).unwrap();
    let pkg_run = run.pkg("python-foo");
    let recorder = Recorder::default();

    let result = run_package_inner(&config, &pkg_run, &recorder);

    assert!(matches!(result, PipelineResult::Failed { .. }));
    let events = recorder.0.lock().unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::StepStarted {
            kind: StepKind::Checkout,
            attempt: 1,
            ..
        }
    )));
    assert_steps_balanced(&events);
}

#[test]
fn no_agent_reports_the_agent_skip_only_once() {
    let mut config = PipelineConfig {
        pkg_name: "foo".into(),
        rpm_name: "python-foo".into(),
        version: "1.0".into(),
        project: "p".into(),
        work_dir: PathBuf::new(),
        state_root: PathBuf::new(),
        rpms_dir: PathBuf::new(),
        distro: "openSUSE_Tumbleweed".into(),
        arch: "x86_64".into(),
        max_attempts: 1,
        budget_usd: 1.0,
        no_agent: true,
        force: false,
        regen: false,
        source_repo: None,
        subprojects: Vec::new(),
        failed_deps: Vec::new(),
        unbuildable_deps: Vec::new(),
        run_id: "test".into(),
        source_urls: Default::default(),
        python_flavors: Default::default(),
        patch_request: None,
        patch_condition: None,
    };
    config.no_agent = true;
    let recorder = Recorder::default();
    let mut step_attempts = Attempts::default();

    // The review gate announces the skip up front, and a later failure
    // path reports it again through `skip_agent`.
    step_attempts.skip(&recorder, StepKind::AgentTurn, SkipReason::NoAgent);
    skip_agent(&recorder, &mut step_attempts, &config, 0, 0.0);

    let events = recorder.0.lock().unwrap();
    let skips = events
        .iter()
        .filter(|e| matches!(e, Event::StepSkipped { .. }))
        .count();
    assert_eq!(skips, 1);
}
