use super::*;
use crate::pipeline::build_round::*;
use crate::pipeline::keep_version::*;
use crate::pipeline::wheel_switch::*;

mod part2;
mod part3;
pub(super) use part2::config;
use part2::*;

#[test]
fn the_post_build_warnings_extract_is_capped_and_skips_progress_lines() {
    let mut log = String::from("[ 12s] compiling foo.c\n");
    for i in 0..200 {
        log.push_str(&format!("[ 99s] foo.c:{i}: warning: unused variable\n"));
    }
    let out = build_warnings_extract(&log);
    assert_eq!(out.lines().count(), POST_REVIEW_WARNING_LINES);
    assert!(!out.contains("compiling foo.c"));
    assert_eq!(build_warnings_extract("all quiet\n"), "");
}

#[test]
fn a_service_that_rewrites_version_is_undone() {
    let before = "Name: pcp\nVersion:        7.2.2\nRelease: 0\n";
    let after = "Name: pcp\nVersion:        5.2.5\nRelease: 0\n";
    assert_eq!(keep_version_line(before, after).as_deref(), Some(before));
    assert_eq!(keep_version_line(before, before), None);
}

#[test]
fn empty_submodule_directories_are_found_in_a_source_tree() {
    let root = tempfile::tempdir().unwrap();
    let tp = root.path().join("third_party");
    for d in ["pybind11", "XNNPACK", "fmt"] {
        std::fs::create_dir_all(tp.join(d)).unwrap();
    }
    std::fs::write(tp.join("fmt/CMakeLists.txt"), "x").unwrap();
    assert_eq!(
        empty_submodule_dirs(root.path()),
        ["third_party/XNNPACK", "third_party/pybind11"]
    );
    let ctx = empty_submodule_context(root.path()).unwrap();
    assert!(ctx.contains("use_upstream_wheel") && ctx.contains("third_party/pybind11"));
    // One empty directory is not enough to give up on the tree.
    std::fs::write(tp.join("XNNPACK/x"), "x").unwrap();
    assert!(empty_submodule_context(root.path()).is_none());
    assert!(empty_submodule_dirs(&root.path().join("missing")).is_empty());
}

#[test]
fn an_old_source_file_is_kept_when_a_new_source_still_names_it() {
    let src = |file: &str| crate::spec::SourceEntry {
        url: format!("https://example.org/{file}"),
        file: file.to_string(),
    };
    let new = [src("keyring.asc"), src("pkg-2.0.tar.gz")];
    // The old slot 0 was `pkg-1.0.tar.gz`; it is superseded.
    assert!(is_superseded_source_file("pkg-1.0.tar.gz", &new));
    // A shifted slot: the old file at index 0 is a file a new source needs.
    assert!(!is_superseded_source_file("keyring.asc", &new));
}

#[test]
fn a_literal_tag_archive_moves_to_the_new_tag() {
    let old = "Name: unordered_dense\nVersion: 4.8.1\nSource0: https://github.com/martinus/unordered_dense/archive/refs/tags/v4.8.1.tar.gz\nSource1: https://example.org/keys/ud.asc\n%description\nx\n";
    let archive = |v: &str| match v {
        "4.8.1" => Some(
            "https://github.com/martinus/unordered_dense/archive/refs/tags/v4.8.1.tar.gz"
                .to_string(),
        ),
        "5.3.3" => Some(
            "https://github.com/martinus/unordered_dense/archive/refs/tags/v5.3.3.tar.gz"
                .to_string(),
        ),
        _ => None,
    };
    let bumped = crate::spec::set_version(old, "5.3.3").unwrap();
    let moved = move_literal_tag_archives("4.8.1", &bumped, "5.3.3", archive).unwrap();
    let sources = crate::spec::expanded_sources(&moved);
    assert_eq!(
        sources[0].url,
        "https://github.com/martinus/unordered_dense/archive/refs/tags/v5.3.3.tar.gz"
    );
    assert_eq!(sources[0].file, "v5.3.3.tar.gz");
    assert_eq!(sources[1].url, "https://example.org/keys/ud.asc");
    // A tag the repository does not list is refused, not left at the old archive.
    let err = move_literal_tag_archives("4.8.1", &bumped, "6.0.0", archive).unwrap_err();
    assert!(err.contains("6.0.0"), "{err}");
    // Nothing names the old version: the spec is left as it is.
    assert_eq!(
        move_literal_tag_archives("9.9.9", &bumped, "5.3.3", archive).unwrap(),
        bumped
    );
}

#[test]
fn a_literal_tag_archive_moves_only_on_its_own_source_line() {
    // The old address is in a comment, and the `.asc` address starts with the
    // old archive's address too: neither is the source that moves.
    let url = |v: &str| format!("https://github.com/o/ud/archive/refs/tags/v{v}.tar.gz");
    let old = format!(
        "Name: ud\nVersion: 4.8.1\n# the old one is {}\nSource0: {}.asc\nSource1: {}#/ud-%{{version}}.tar.gz\n%description\nx\n",
        url("4.8.1"),
        url("4.8.1"),
        url("4.8.1"),
    );
    let bumped = crate::spec::set_version(&old, "5.3.3").unwrap();
    let calls = std::cell::Cell::new(0);
    let archive = |v: &str| {
        calls.set(calls.get() + 1);
        match v {
            "4.8.1" => Some(url("4.8.1")),
            "5.3.3" => Some(url("5.3.3")),
            _ => None,
        }
    };
    let moved = move_literal_tag_archives("4.8.1", &bumped, "5.3.3", archive).unwrap();
    assert!(
        moved.contains(&format!("# the old one is {}\n", url("4.8.1"))),
        "{moved}"
    );
    assert!(
        moved.contains(&format!("Source0: {}.asc\n", url("4.8.1"))),
        "{moved}"
    );
    assert!(
        moved.contains(&format!(
            "Source1: {}#/ud-%{{version}}.tar.gz\n",
            url("5.3.3")
        )),
        "{moved}"
    );
    let sources = crate::spec::expanded_sources(&moved);
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[1].url, url("5.3.3"));
    assert_eq!(sources[1].file, "ud-5.3.3.tar.gz");
    // Each version's archive is asked for once, not once per candidate.
    assert!(calls.get() <= 2, "asked {} times", calls.get());
}

#[test]
fn a_literal_tag_archive_written_with_a_macro_is_refused() {
    let old = "Name: ud\nVersion: 4.8.1\nSource0: https://example.org/%{name}/refs/v4.8.1.tar.gz\n%description\nx\n";
    let archive = |v: &str| match v {
        "4.8.1" => Some("https://example.org/ud/refs/v4.8.1.tar.gz".to_string()),
        "5.3.3" => Some("https://example.org/ud/refs/v5.3.3.tar.gz".to_string()),
        _ => None,
    };
    let err = move_literal_tag_archives("4.8.1", old, "5.3.3", archive).unwrap_err();
    assert!(err.contains("macro"), "{err}");
}

#[test]
fn the_pipelines_own_wheel_switch_gives_a_reason_the_gate_accepts() {
    let root = tempfile::tempdir().unwrap();
    for d in ["pybind11", "XNNPACK"] {
        std::fs::create_dir_all(root.path().join("third_party").join(d)).unwrap();
    }
    let why = empty_submodule_why(root.path());
    assert!(why.contains("third_party/XNNPACK") && validate_wheel_switch_why(&why).is_ok());
    let outcome = pipeline_wheel_outcome(&why);
    assert!(matches!(
        outcome.action,
        crate::agent::Action::UseUpstreamWheel { why: ref w } if *w == why
    ));
}

#[test]
fn a_patch_for_another_releases_tree_is_stale() {
    let diff = "--- pytorch-2.3.1/setup.py\t2024\n+++ pytorch-2.3.1/setup.py\n@@ -1 +1 @@\n";
    assert_eq!(
        stale_patch_prefix(diff, "torch-2.14.0", "2.14.0").as_deref(),
        Some("pytorch-2.3.1")
    );
    assert!(stale_patch_prefix(diff, "pytorch-2.3.1", "2.3.1").is_none());
    let ab = "--- a/setup.py\n+++ b/setup.py\n";
    assert!(stale_patch_prefix(ab, "torch-2.14.0", "2.14.0").is_none());
    let same = "--- torch-2.14.0/setup.py\n+++ torch-2.14.0/setup.py\n";
    assert!(stale_patch_prefix(same, "torch-2.14.0", "2.14.0").is_none());
}

#[test]
fn stale_patches_are_found_and_dropped_from_the_spec() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("old.patch"),
        "--- pytorch-2.3.1/a\n+++ pytorch-2.3.1/a\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("ok.patch"), "--- a/a\n+++ b/a\n").unwrap();
    let spec =
        "Name: x\nPatch0: old.patch\nPatch1: ok.patch\n%prep\n%patch0 -p1\n%patch -P 1 -p1\n";
    let stale = stale_patches(spec, dir.path(), "torch-2.14.0", "2.14.0");
    assert_eq!(
        stale,
        [("old.patch".to_string(), "pytorch-2.3.1".to_string())]
    );
    assert_eq!(
        without_patches(spec, &["old.patch".to_string()]),
        "Name: x\nPatch1: ok.patch\n%prep\n%patch -P 1 -p1\n"
    );
}

#[test]
fn without_patches_removes_every_form_of_the_applied_patch_only() {
    let spec = "Name: x\nPatch: bare.patch\nPatch1: spaced.patch\nPatch2: glued.patch\nPatch3: keep.patch\n%prep\n%patch -p1\n%patch -P 1 -p1\n%patch -P2 -p1\n%patch3 -p1\n%patchfoo\n";
    let kept = without_patches(
        spec,
        &[
            "bare.patch".to_string(),
            "spaced.patch".to_string(),
            "glued.patch".to_string(),
        ],
    );
    assert_eq!(
        kept,
        "Name: x\nPatch3: keep.patch\n%prep\n%patch3 -p1\n%patchfoo\n"
    );
}

#[test]
fn applied_patch_number_reads_each_macro_form() {
    assert_eq!(applied_patch_number("%patch -p1").as_deref(), Some("0"));
    assert_eq!(applied_patch_number("%patch").as_deref(), Some("0"));
    assert_eq!(applied_patch_number("%patch0 -p1").as_deref(), Some("0"));
    assert_eq!(
        applied_patch_number("%patch -P 12 -p1").as_deref(),
        Some("12")
    );
    assert_eq!(applied_patch_number("%patch -P3").as_deref(), Some("3"));
    // The number may follow another option, or come after the -p level.
    assert_eq!(
        applied_patch_number("%patch -p1 -P 2").as_deref(),
        Some("2")
    );
    assert_eq!(
        applied_patch_number("%patch -P 2 -p1").as_deref(),
        Some("2")
    );
    assert_eq!(applied_patch_number("%patch -p1 -P2").as_deref(), Some("2"));
    assert_eq!(applied_patch_number("%patchfoo"), None);
    assert_eq!(applied_patch_number("echo %patch1"), None);
}

#[test]
fn the_third_identical_spec_rejection_fails_the_step() {
    let spec = PathBuf::from("/nonexistent/hygiene-reject/python-x.spec");
    let other = PathBuf::from("/nonexistent/hygiene-reject/python-y.spec");
    note_spec_rejection(&spec, "patch does not apply");
    note_spec_rejection(&spec, "patch does not apply");
    note_spec_rejection(&spec, "a different error");
    assert!(repeated_spec_rejection(&spec).is_none());
    note_spec_rejection(&spec, "patch does not apply");
    let msg = repeated_spec_rejection(&spec).unwrap();
    assert!(
        msg.contains("3 times") && msg.contains("patch does not apply"),
        "{msg}"
    );
    assert!(repeated_spec_rejection(&other).is_none());
    clear_spec_rejections(&spec);
    assert!(repeated_spec_rejection(&spec).is_none());
}

#[test]
fn a_stale_or_already_read_build_log_skips_the_prefix_turn() {
    use std::time::{Duration, SystemTime};
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let t1 = t0 + Duration::from_secs(10);
    // Spec edited after the build.
    assert!(stale_build_log_reason(Some(t1), Some(t0), "boom", None).is_some());
    // Log newer than the spec, not read before.
    assert!(stale_build_log_reason(Some(t0), Some(t1), "boom", None).is_none());
    // Same content as the last turn read.
    assert!(stale_build_log_reason(Some(t0), Some(t1), "boom", Some(hash_spec("boom"))).is_some());
    assert!(stale_build_log_reason(Some(t0), Some(t1), "bang", Some(hash_spec("boom"))).is_none());
    // Unknown times never block the turn.
    assert!(stale_build_log_reason(None, None, "boom", None).is_none());
}

#[test]
fn a_failed_ladder_is_remembered_for_the_same_spec_only() {
    let st = PkgState {
        ladder_spec_hash: Some(hash_spec("spec A")),
        ladder_reason: Some("turn 0: needs a compiler".into()),
        ..Default::default()
    };
    assert_eq!(st.failed_ladder("spec A"), Some("turn 0: needs a compiler"));
    assert_eq!(st.failed_ladder("spec B"), None);
    assert_eq!(PkgState::default().failed_ladder("spec A"), None);
    let back: PkgState = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
    assert_eq!(
        back.failed_ladder("spec A"),
        Some("turn 0: needs a compiler")
    );
    // An old state.json that recorded a block still loads; the fields are gone.
    let old: PkgState =
        serde_json::from_str(r#"{"blocked_spec_hash": 5, "blocked_reason": "x"}"#).unwrap();
    assert_eq!(old.ladder_spec_hash, None);
}

fn write_state(root: &Path, name: &str, outcome: &str, reason: Option<&str>) {
    let ws = Workspace::open(root, name).unwrap();
    PkgState::update(&ws, |s| {
        s.last_outcome = Some(outcome.into());
        s.last_failure_reason = reason.map(String::from);
    })
    .unwrap();
}

fn outcome(root: &Path, name: &str) -> Option<String> {
    PkgState::load(&Workspace::open(root, name).unwrap()).last_outcome
}

#[test]
fn startup_reset_frees_dead_running_and_satisfied_missing_deps() {
    let root = tempfile::tempdir().unwrap();
    write_state(root.path(), "python-dead", "running", None);
    write_state(root.path(), "python-live", "running", None);
    write_state(root.path(), "ffmpeg-8", "success", None);
    write_state(
        root.path(),
        "python-tc",
        "missing_deps",
        Some("waiting on dependencies: ffmpeg-8"),
    );
    write_state(
        root.path(),
        "python-wait",
        "missing_deps",
        Some("waiting on dependencies: ffmpeg-8, nothere"),
    );
    // A live build holds its lock.
    let live_lock = Workspace::open(root.path(), "python-live")
        .unwrap()
        .dir()
        .join("build.lock");
    let _held = ap_core::proc_lock::try_lock(&live_lock).unwrap();
    let changed = reconcile_state(root.path(), |_| true);
    assert_eq!(
        outcome(root.path(), "python-dead").as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        outcome(root.path(), "python-live").as_deref(),
        Some("running")
    );
    assert_eq!(
        outcome(root.path(), "python-tc").as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        outcome(root.path(), "python-wait").as_deref(),
        Some("missing_deps")
    );
    assert_eq!(outcome(root.path(), "ffmpeg-8").as_deref(), Some("success"));
    assert_eq!(changed.len(), 2, "{changed:?}");
}

#[test]
fn dependents_of_a_given_up_package_get_their_own_outcome_and_are_freed_with_it() {
    let node = |name: &str, version: &str, requires: &[&str]| ap_ecosystem::pip::Node {
        name: name.into(),
        version: version.into(),
        sha256: String::new(),
        requires_dist: requires.iter().map(|s| s.to_string()).collect(),
        requires_python: None,
        is_requested: false,
        is_yanked: false,
    };
    let nodes = vec![
        node("torch", "1", &[]),
        node("mid", "1", &["torch>=1"]),
        node("top", "1", &["mid", "ok"]),
        node("ok", "1", &[]),
        node("other", "1", &[]),
    ];
    let resolvable = vec![false, false, false, true, false];
    let failed: std::collections::HashSet<String> =
        [osc::fuzzy_key(&osc::rpm_name("torch"))].into();
    let found = blocked_dependents(&nodes, &resolvable, &failed);
    assert_eq!(found, vec![(1, 0), (2, 0)]);

    let root = tempfile::tempdir().unwrap();
    write_state(root.path(), "python-torch", "failed", Some("needs a GPU"));
    write_state(
        root.path(),
        "python-top",
        "missing_deps",
        Some("waiting on dependencies: x"),
    );
    write_state(root.path(), "python-mid", "interrupted", None);
    let edges = vec![
        ("python-mid".to_string(), "python-torch".to_string()),
        ("python-top".to_string(), "python-torch".to_string()),
    ];
    sync_blocked_waiters(root.path(), &edges, &|_| true);
    assert_eq!(
        outcome(root.path(), "python-mid").as_deref(),
        Some(WAITING_ON_FAILED)
    );
    let ws = Workspace::open(root.path(), "python-top").unwrap();
    assert_eq!(
        PkgState::load(&ws).last_failure_reason.as_deref(),
        Some("waiting on failed package: python-torch")
    );
    // The same edges again: nothing changes.
    let (again, _) = sync_blocked_waiters(root.path(), &edges, &|_| true);
    assert!(again.is_empty());
    // The graph is not known at startup: the dependents are ready again.
    let changed = reconcile_state(root.path(), |_| true);
    assert_eq!(changed.len(), 2, "{changed:?}");
    assert_eq!(
        outcome(root.path(), "python-mid").as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        outcome(root.path(), "python-top").as_deref(),
        Some("interrupted")
    );
}

#[test]
fn missing_deps_dependents_wait_on_a_given_up_package_transitively_and_are_freed() {
    let root = tempfile::tempdir().unwrap();
    write_state(root.path(), "python-torch", "failed", Some("needs a GPU"));
    write_state(root.path(), "python-other", "interrupted", None);
    write_state(
        root.path(),
        "python-mid",
        "missing_deps",
        Some("waiting on dependencies: torch"),
    );
    write_state(
        root.path(),
        "python-top",
        "missing_deps",
        Some("waiting on dependencies: ok, python-mid"),
    );
    write_state(
        root.path(),
        "python-free",
        "missing_deps",
        Some("waiting on dependencies: nothere"),
    );
    write_state(root.path(), "python-ok", "success", None);
    let edges = vec![("python-other".to_string(), "python-torch".to_string())];
    let (changed, _) = sync_blocked_waiters(root.path(), &edges, &|_| true);
    assert_eq!(changed.len(), 3, "{changed:?}");
    assert_eq!(
        outcome(root.path(), "python-mid").as_deref(),
        Some(WAITING_ON_FAILED)
    );
    assert_eq!(
        outcome(root.path(), "python-top").as_deref(),
        Some(WAITING_ON_FAILED)
    );
    assert_eq!(
        outcome(root.path(), "python-free").as_deref(),
        Some("missing_deps")
    );
    let ws = Workspace::open(root.path(), "python-top").unwrap();
    assert_eq!(
        PkgState::load(&ws).last_failure_reason.as_deref(),
        Some("waiting on failed package: python-torch")
    );
    // Idempotent.
    let (again, _) = sync_blocked_waiters(root.path(), &edges, &|_| true);
    assert!(again.is_empty());
    write_state(root.path(), "python-torch", "success", None);
    let changed = reconcile_state(root.path(), |_| true);
    assert_eq!(changed.len(), 3, "{changed:?}");
    assert_eq!(
        outcome(root.path(), "python-top").as_deref(),
        Some("interrupted")
    );
}

#[test]
fn a_transient_failure_is_recorded_as_interrupted_and_a_real_one_as_failed() {
    let failed = |kind| PipelineResult::Failed {
        rpm_name: "python-x".into(),
        reason: "r".into(),
        kind,
        remedy: None,
        note: None,
    };
    assert_eq!(
        recorded_outcome(&failed(FailureKind::Transient)).unwrap().0,
        "interrupted"
    );
    // A full disk is re-runnable like the network failures, not a defect.
    assert_eq!(
        recorded_outcome(&failed(FailureKind::DiskFull)).unwrap().0,
        "interrupted"
    );
    assert_eq!(
        recorded_outcome(&failed(FailureKind::Other)).unwrap().0,
        "failed"
    );
}

#[test]
fn wheel_provenance_left_in_a_checkout_is_copied_to_the_state_directory_once() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("co");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("wheel-origin.txt"), "from the checkout").unwrap();
    std::fs::write(checkout.join("python-w.spec.pre-wheel-switch"), "old spec").unwrap();
    let ws = Workspace::open(&root.path().join("state"), "python-w").unwrap();
    let quiet = Warnings(Default::default());
    migrate_wheel_provenance(&checkout, &ws, "python-w", &quiet);
    assert_eq!(
        std::fs::read_to_string(ws.dir().join("wheel-origin.txt")).unwrap(),
        "from the checkout"
    );
    assert_eq!(
        std::fs::read_to_string(ws.dir().join("python-w.spec.pre-wheel-switch")).unwrap(),
        "old spec"
    );
    // The state's own record is not overwritten, and the checkout keeps its copy.
    std::fs::write(ws.dir().join("wheel-origin.txt"), "newer").unwrap();
    migrate_wheel_provenance(&checkout, &ws, "python-w", &quiet);
    assert_eq!(
        std::fs::read_to_string(ws.dir().join("wheel-origin.txt")).unwrap(),
        "newer"
    );
    assert!(checkout.join("wheel-origin.txt").is_file());
    assert!(quiet.0.lock().unwrap().is_empty());
}

#[test]
fn transient_failures_are_retried_a_bounded_number_of_times() {
    let calls = std::cell::Cell::new(0);
    let zero = std::time::Duration::ZERO;
    // A dropped connection that clears on the third try.
    let r: Result<u32, String> = retry_transient(
        3,
        zero,
        |e: &String| e.contains("reset"),
        || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err("connection reset".into())
            } else {
                Ok(7)
            }
        },
    );
    assert_eq!((r, calls.get()), (Ok(7), 3));
    // One that never clears stops at the bound.
    calls.set(0);
    let r: Result<u32, String> = retry_transient(
        3,
        zero,
        |_| true,
        || {
            calls.set(calls.get() + 1);
            Err("connection reset".into())
        },
    );
    assert_eq!((r.is_err(), calls.get()), (true, 3));
    // A real failure is not retried.
    calls.set(0);
    let r: Result<u32, String> = retry_transient(
        3,
        zero,
        |_| false,
        || {
            calls.set(calls.get() + 1);
            Err("yanked".into())
        },
    );
    assert_eq!((r.is_err(), calls.get()), (true, 1));
}

#[test]
fn an_excluded_build_is_recorded_as_skipped_not_failed() {
    let excluded = PipelineResult::Failed {
        rpm_name: "lsvpd".into(),
        reason: "OBS: build is excluded for x86_64 in project config".into(),
        kind: FailureKind::Excluded,
        remedy: None,
        note: None,
    };
    let (outcome, reason) = recorded_outcome(&excluded).unwrap();
    assert_eq!(outcome, "skipped");
    assert!(reason.contains("excluded"), "{reason}");
}

#[test]
fn a_skip_from_an_earlier_run_is_cleared_when_the_package_is_worked_on_again() {
    assert!(is_earlier_run_outcome(Some("skipped")));
    assert!(is_earlier_run_outcome(Some("failed")));
    assert!(!is_earlier_run_outcome(Some("success")));
    assert!(!is_earlier_run_outcome(None));
}
