use super::*;
use crate::pipeline::dep_edit::*;
use crate::pipeline::run::*;
use crate::pipeline::slots::*;
use crate::pipeline::target::*;

#[test]
fn only_success_is_a_built_state() {
    let make = |outcome: &str| PkgState {
        last_built_version: Some("1.0".into()),
        last_outcome: Some(outcome.into()),
        last_failure_reason: None,
        reviewed_version: None,
        reviewed_spec_hash: None,
        cumulative_spent_usd: 0.0,
        cumulative_spent_run_id: None,
        ..Default::default()
    };
    assert!(make("success").is_built("1.0"));
    assert!(!make("failed").is_built("1.0"));
    assert!(!make("wheel-only").is_built("1.0"));
    assert!(!make("something-else").is_built("1.0"));
}

#[test]
fn pkg_state_round_trips_and_tolerates_the_missing_failure_reason_field() {
    let legacy = r#"{"last_built_version":"1.2.3","last_outcome":"success"}"#;
    let s: PkgState = serde_json::from_str(legacy)
        .expect("legacy state.json without last_failure_reason must still parse");
    assert_eq!(s.last_built_version.as_deref(), Some("1.2.3"));
    assert_eq!(s.last_outcome.as_deref(), Some("success"));
    assert!(s.last_failure_reason.is_none());

    let fresh = PkgState {
        last_built_version: Some("1.2.3".into()),
        last_outcome: Some("failed".into()),
        last_failure_reason: Some("agent error: claude exited 1".into()),
        reviewed_version: None,
        reviewed_spec_hash: None,
        cumulative_spent_usd: 0.0,
        cumulative_spent_run_id: None,
        ..Default::default()
    };
    let json = serde_json::to_string(&fresh).unwrap();
    let back: PkgState = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.last_failure_reason.as_deref(),
        Some("agent error: claude exited 1")
    );
    assert!(
        !back.is_built("1.2.3"),
        "`failed` must never satisfy is_built"
    );
}

/// `Succeeded` is the only hard "available right now" status; the three
/// transient OBS states (`Blocked`/`Building`/`Scheduled`) are kept in to
/// avoid stampeding work OBS is already doing. Everything else — the
/// explicit terminal failures, user-disabled/excluded sources, and the
/// catch-all `Unknown` — is not available for a dependent build to link
/// against and must not be treated as resolvable.
#[test]
fn only_succeeded_and_in_flight_statuses_count_as_resolvable() {
    for ok in [
        crate::osc::BuildStatus::Succeeded,
        crate::osc::BuildStatus::Blocked,
        crate::osc::BuildStatus::Building,
        crate::osc::BuildStatus::Scheduled,
    ] {
        assert!(
            status_is_resolvable(Some(&ok)),
            "{ok:?} should still count as resolvable"
        );
    }
    for not_ok in [
        crate::osc::BuildStatus::Failed,
        crate::osc::BuildStatus::Unresolvable,
        crate::osc::BuildStatus::Broken,
        crate::osc::BuildStatus::Disabled,
        crate::osc::BuildStatus::Excluded,
        crate::osc::BuildStatus::Unknown("x".into()),
    ] {
        assert!(
            !status_is_resolvable(Some(&not_ok)),
            "{not_ok:?} must not count as resolvable \
                 (Oct 6 finding #1: this was silently green)"
        );
    }
}

/// A package that is in the OBS project source listing but has no build
/// status entry at all for the requested distro/arch previously counted
/// as resolvable (the `!matches!(... Some(Failed|Unresolvable|Broken))`
/// returned true on `None`), making every "I added a source but haven't
/// ever built it for this arch" package look available to its
/// dependents. Must now be treated as unresolved.
#[test]
fn a_package_with_no_build_status_entry_is_not_resolvable() {
    assert!(!status_is_resolvable(None));
}

/// Oct 6 review finding #4: a `UseUpstreamWheel` with a bare / empty
/// `why` previously swapped the recipe silently. Enforce a minimum
/// shape so the audit record has at least a usable sentence in it.
#[test]
fn use_upstream_wheel_rejects_a_trivially_short_why() {
    for empty in ["", " ", "idk", "no reason", "needs nvcc"] {
        assert!(
            validate_wheel_switch_why(empty).is_err(),
            "{empty:?} must be rejected as audit evidence"
        );
    }
}

/// A real-shape `why` matching the convention 28 Exception 4 example
/// (quoted build.log line + named toolchain) must pass.
#[test]
fn use_upstream_wheel_accepts_a_real_shape_why() {
    for ok in [
        "nvcc: command not found at build.log:847, needed by find_package(CUDAToolkit REQUIRED)",
        "The source needs NVIDIA's proprietary nvcc toolchain; find_package(CUDAToolkit REQUIRED) failed",
    ] {
        assert!(
            validate_wheel_switch_why(ok).is_ok(),
            "{ok:?} should be accepted — this is the exact evidence shape \
                 convention 28 Exception 4 asks for"
        );
    }
}

#[test]
fn explicit_target_matches_regardless_of_dot_vs_hyphen_spelling() {
    // The real incident this guards against: the closure holds the node
    // under its PyPI name `backports.zstd`, but the already-committed
    // OBS package is `python-backports-zstd` (hyphenated per convention
    // 2). A caller passing either `backports-zstd` or
    // `python-backports.zstd` must land on the same node instead of
    // `find_explicit_target` reporting no match and the caller going on
    // to check out a brand-new, wrongly-named package.
    let nodes = vec![fake_node("backports.zstd"), fake_node("other-pkg")];

    for spelling in [
        "backports.zstd",
        "backports-zstd",
        "python-backports-zstd",
        "BACKPORTS_ZSTD",
    ] {
        let found = find_explicit_target(&nodes, spelling)
            .unwrap_or_else(|e| panic!("expected a match for {spelling:?}: {e}"));
        assert_eq!(found.name, "backports.zstd");
    }
}

#[test]
fn explicit_target_error_lists_available_names() {
    let nodes = vec![fake_node("torchcodec"), fake_node("backports.zstd")];
    let err = find_explicit_target(&nodes, "nonexistent").unwrap_err();
    assert!(
        err.contains("torchcodec"),
        "error should list available names: {err}"
    );
    assert!(
        err.contains("backports.zstd"),
        "error should list available names: {err}"
    );
}

#[test]
fn rate_limit_pause_doubles_and_caps_at_a_minute() {
    assert_eq!(rate_limit_pause(1).as_secs(), 2);
    assert_eq!(rate_limit_pause(2).as_secs(), 4);
    assert_eq!(rate_limit_pause(4).as_secs(), 16);
    assert_eq!(rate_limit_pause(50).as_secs(), 60);
}

#[test]
fn claiming_a_new_run_resets_spend_and_attempts_but_keeps_the_same_run() {
    let mut st = PkgState {
        cumulative_spent_usd: 3.5,
        cumulative_attempts: 4,
        cumulative_spent_run_id: Some("run-a".to_string()),
        ..PkgState::default()
    };
    claim_run(&mut st, "run-a");
    assert_eq!((st.cumulative_spent_usd, st.cumulative_attempts), (3.5, 4));
    claim_run(&mut st, "run-b");
    assert_eq!((st.cumulative_spent_usd, st.cumulative_attempts), (0.0, 0));
    assert_eq!(st.cumulative_spent_run_id.as_deref(), Some("run-b"));
}

#[test]
fn spend_from_an_earlier_run_does_not_count_against_this_one() {
    let state = PkgState {
        cumulative_spent_usd: 3.5,
        cumulative_spent_run_id: Some("run-a".to_string()),
        ..PkgState::default()
    };
    assert_eq!(spent_in_run(&state, "run-a"), 3.5);
    assert_eq!(spent_in_run(&state, "run-b"), 0.0);
    assert_eq!(spent_in_run(&PkgState::default(), "run-a"), 0.0);
}

/// Real shape from `triton`/`python-hf-xet`/`python-torchcodec`: a plain
/// GCC compiler diagnostic in `%build` is neither a Python traceback nor
/// a `script: line N:` shell error, so its genre is `Unknown` — but the
/// phase is unambiguous (`error: Bad exit status from ... (%build)`
/// names it), and this used to get thrown away by switching on genre
/// instead of phase, reporting `Unclassified` for a perfectly ordinary
/// `%build` failure.
#[test]
fn a_compiler_error_in_build_is_build_failed_not_unclassified() {
    let raw = "\
[    1s] Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.abc123
[    2s] + cmake --build .
[   64s] /build/triton-0.9/src/utils.cpp:39:34: error: 'PyLong_AsUnsignedLongLongAndOverflow' was not declared in this scope
[   64s] make[1]: *** [Makefile:149: all] Error 2
[   64s] error: Bad exit status from /var/tmp/rpm-tmp.abc123 (%build)
";
    let mapper = crate::log::PathMapper::with_sdist_root("triton-0.9");
    let extracted = crate::log::extract(raw, &mapper);
    assert_eq!(
        extracted.failure.as_ref().map(|f| f.genre),
        Some(crate::log::Genre::Unknown),
        "fixture must reproduce the Unknown-genre shape this test guards against"
    );
    match classify_failure(&extracted) {
        ap_core::event::BuildFailure::BuildFailed => {}
        other => panic!("expected BuildFailed, got {other:?}"),
    }
}

/// Real shape from `drogon`: OBS reported the build as Failed but no
/// build.log was ever produced (the worker aborted during buildroot
/// resolution). `error_lines` is empty, so the old `error_lines.first()`
/// pick returned an empty `raw` — the TUI then just showed
/// "classification: Unclassified { raw: '' }" with no clue what happened.
/// Now we fall back to the log tail (or an explicit "no build log"
/// placeholder) so the operator sees something they can act on.
#[test]
fn unclassified_falls_back_to_log_tail_when_no_error_lines() {
    // Shaped like an aborted buildroot resolution: lots of chatter, no
    // `error:` line, no Python traceback, no `Bad exit status`.
    let raw = "\
[    0s] expanding package dependencies for openSUSE_Tumbleweed/x86_64 ...
[    1s] cannot find libtrantor-devel in any configured repository
[    1s] worker aborted
";
    let mapper = crate::log::PathMapper::with_sdist_root("drogon-1.9.13");
    let extracted = crate::log::extract(raw, &mapper);
    assert!(
        extracted.error_lines.is_empty(),
        "fixture must reproduce the no-error-lines shape"
    );
    match classify_failure(&extracted) {
        ap_core::event::BuildFailure::Unclassified { raw } => {
            assert!(!raw.is_empty(), "raw should not fall back to empty");
            assert!(
                raw.contains("libtrantor-devel") || raw.contains("worker aborted"),
                "raw should carry a line from the log tail; got {raw:?}"
            );
        }
        other => panic!("expected Unclassified, got {other:?}"),
    }
}

#[test]
fn unclassified_reports_the_missing_log_explicitly_when_log_is_empty() {
    let mapper = crate::log::PathMapper::with_sdist_root("foo-1.0");
    let extracted = crate::log::extract("", &mapper);
    match classify_failure(&extracted) {
        ap_core::event::BuildFailure::Unclassified { raw } => {
            assert!(
                raw.contains("no build log"),
                "empty-log raw should name the condition, got {raw:?}"
            );
        }
        other => panic!("expected Unclassified, got {other:?}"),
    }
}

/// A phase other than the 4 explicitly named ones (`%generate_buildrequires`
/// is a real case for pyproject-rpm-macros' dynamic BuildRequires
/// resolution) used to fall all the way through to `Unclassified`,
/// discarding the phase `failure.phase` already captured.
#[test]
fn an_unnamed_section_failure_is_prep_failed_not_unclassified() {
    let raw = "\
[    1s] Executing(%generate_buildrequires): /usr/bin/bash -e /var/tmp/rpm-tmp.abc123
[    2s] + pyproject_buildrequires
[    3s] ModuleNotFoundError: No module named 'setuptools_scm'
[    3s] error: Bad exit status from /var/tmp/rpm-tmp.abc123 (%generate_buildrequires)
";
    let mapper = crate::log::PathMapper::with_sdist_root("foo-1.0");
    let extracted = crate::log::extract(raw, &mapper);
    match classify_failure(&extracted) {
        ap_core::event::BuildFailure::PrepFailed => {}
        other => panic!("expected PrepFailed, got {other:?}"),
    }
}

#[test]
fn diff_counts_reports_both_directions() {
    // The old code hard-coded lines_removed to 0 and derived lines_added
    // from the line-count delta, so a pure replacement reported +0 -0.
    let diff = unified_diff("a\nb\nc\n", "a\nX\nc\n");
    assert_eq!(diff_counts(&diff), (1, 1));

    let diff = unified_diff("a\n", "a\nb\nc\n");
    assert_eq!(diff_counts(&diff), (2, 0));

    assert_eq!(diff_counts(&unified_diff("a\n", "a\n")), (0, 0));
}

#[test]
fn dep_names_skip_extras_and_strip_specifiers() {
    let reqs = vec![
        "pytest>=7.0; extra == 'test'".to_string(),
        "requests[socks] >= 2.0".to_string(),
        "typing-extensions".to_string(),
    ];
    // `normalize_pkg_name` (== `osc::fuzzy_key`) strips separators
    // entirely rather than picking one canonical spelling — it's a
    // comparison-only key, never displayed or built from.
    assert_eq!(
        dep_names_from_requires_dist(&reqs),
        vec!["requests", "typingextensions"]
    );
}

#[test]
fn dep_names_match_regardless_of_the_original_dependencys_separator_style() {
    // A `ruamel.yaml`-style dotted node name and a `requires_dist` entry
    // that spells the same project with a hyphen or underscore instead
    // must normalize to the same key, or the dependency-graph edge
    // between them silently fails to resolve.
    let reqs = vec!["ruamel-yaml".to_string(), "ruamel_yaml".to_string()];
    assert_eq!(
        normalize_pkg_name("ruamel.yaml"),
        normalize_pkg_name("ruamel-yaml")
    );
    for name in dep_names_from_requires_dist(&reqs) {
        assert_eq!(name, normalize_pkg_name("ruamel.yaml"));
    }
}

#[test]
fn missing_source_urls_lists_http_sources_whose_file_is_missing() {
    let spec = "\
Name:           foo
Source0:        https://github.com/o/r/archive/refs/tags/v1.2.tar.gz#/r-1.2.tar.gz
Source1:        http://example.com/x/brotli-1.2.0.tar.gz
Source2:        https://example.com/have-1.0.tar.gz
Source3:        registry.tar.zst
Source4:        ftp://example.com/old.tar.gz
Source5:        file:///etc/passwd
Patch0:         https://example.com/fix.patch
";
    let missing: Vec<String> = [
        "r-1.2.tar.gz",
        "brotli-1.2.0.tar.gz",
        "registry.tar.zst",
        "old.tar.gz",
        "passwd",
        "fix.patch",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        missing_source_urls(spec, &missing),
        vec![
            "https://github.com/o/r/archive/refs/tags/v1.2.tar.gz#/r-1.2.tar.gz",
            "http://example.com/x/brotli-1.2.0.tar.gz",
        ]
    );
}

#[test]
fn download_service_is_written_for_a_missing_url_source_without_a_service() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Source0:        https://example.com/InstantTensor-0.2.2.tar.gz\n";
    let missing = vec!["InstantTensor-0.2.2.tar.gz".to_string()];
    assert!(write_download_service(spec, &missing, dir.path()));
    let written = std::fs::read_to_string(dir.path().join("_service")).unwrap();
    assert!(written.contains("name=\"download_files\""));
    assert!(written.contains("mode=\"manual\""));
    assert!(written.starts_with("<services>") && written.trim_end().ends_with("</services>"));
}

#[test]
fn download_service_leaves_an_existing_service_alone() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("_service"), "mine").unwrap();
    let spec = "Source0:        https://example.com/a-1.tar.gz\n";
    let missing = vec!["a-1.tar.gz".to_string()];
    assert!(!write_download_service(spec, &missing, dir.path()));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("_service")).unwrap(),
        "mine"
    );
}

#[test]
fn download_service_is_not_written_for_a_source_that_is_not_a_url() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Source0:        a-1.tar.gz\nSource1:        ftp://example.com/b.tar.gz\n\
                    Patch0:         https://example.com/c.patch\n";
    let missing: Vec<String> = ["a-1.tar.gz", "b.tar.gz", "c.patch"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(!write_download_service(spec, &missing, dir.path()));
    assert!(!dir.path().join("_service").exists());
    assert!(!write_download_service(spec, &[], dir.path()));
}

pub(super) fn view(own: &[(&str, osc::BuildStatus)], linked: &[&str]) -> ProjectView {
    ProjectView {
        listing: std::sync::Arc::new(own.iter().map(|(n, _)| n.to_string()).collect()),
        statuses: own
            .iter()
            .map(|(n, s)| (n.to_string(), s.clone()))
            .collect(),
        linked: vec![(
            "devel:languages:python".to_string(),
            std::sync::Arc::new(linked.iter().map(|n| n.to_string()).collect()),
        )],
    }
}

#[test]
fn added_python_dependencies_lists_only_new_python_packages() {
    let before = "\
BuildRequires:  %{python_module pip}
BuildRequires:  python-old
Requires:       python-runtime-dep
";
    let after = "\
BuildRequires:  %{python_module pip}
BuildRequires:  python-old
BuildRequires:  %{python_module cuda_bindings >= 13}
BuildRequires:  python3-newbuild
BuildRequires:  python3-devel
BuildRequires:  python-foo-devel
BuildRequires:  gcc-c++
BuildRequires:  pkgconfig(libfoo)
BuildRequires:  %{python_module %{name}}
Requires:       python-runtime-dep
Requires:       python-added-runtime >= 1.0
Requires:       python-Old
";
    assert_eq!(
        added_python_dependencies(before, after),
        vec![
            "python-cuda_bindings",
            "python-newbuild",
            "python-devel",
            "python-added-runtime"
        ]
    );
    // Nothing changed, or something removed: nothing added.
    assert!(added_python_dependencies(after, before).is_empty());
    assert!(added_python_dependencies(after, after).is_empty());
}

#[test]
fn added_python_dependencies_triage_sets_aside_interpreter_packages() {
    let added = vec![
        "python-devel".to_string(),
        "python-cuda_bindings".to_string(),
    ];
    let (named, skipped) = crate::depclass::triage_all(&added, &|_| false);
    assert_eq!(named, vec!["cuda_bindings"]);
    assert_eq!(skipped.len(), 1);
}

#[test]
fn not_shipped_keeps_only_what_no_project_ships() {
    use osc::BuildStatus::*;
    let v = view(
        &[
            ("python-built", Succeeded),
            ("python-failing", Failed),
            ("python-building", Building),
            ("python-blocked", Blocked),
        ],
        &["python-linked-one", "python-Mixed_Case"],
    );
    let names: Vec<String> = [
        "built",
        "failing",
        "building",
        "blocked",
        "linked_one",
        "mixed-case",
        "cuda-bindings",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(not_shipped(names, &v), vec!["failing", "cuda-bindings"]);
}

#[test]
fn missing_source_files_flags_a_source_or_patch_not_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("foo-1.0.tar.gz"), b"").unwrap();

    let spec = "\
Name:           foo
Source:         https://example.com/foo-1.0.tar.gz
Source1:        registry.tar.zst
Patch0:         0001-fix.patch
";
    let mut missing = missing_source_files(spec, dir.path());
    missing.sort();
    assert_eq!(missing, vec!["0001-fix.patch", "registry.tar.zst"]);
}

#[test]
fn missing_source_files_is_empty_when_everything_is_present() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("foo-1.0.tar.gz"), b"").unwrap();

    let spec = "Source: https://example.com/foo-1.0.tar.gz\n";
    assert!(missing_source_files(spec, dir.path()).is_empty());
}

/// Reproduces the python-python-docx bug: a `series` file left over from
/// an earlier quilt session names a patch the current spec no longer
/// declares. `%autosetup -p1` then chokes trying to apply it during
/// `%prep` even though the spec currently has zero `Patch:` lines.
#[test]
fn sync_quilt_series_drops_a_patch_the_spec_no_longer_declares() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("series"), b"0001-orphaned.patch\n").unwrap();

    sync_quilt_series("Name: foo\n", dir.path());

    assert!(
        !dir.path().join("series").exists(),
        "stale series file must be removed when the spec declares no patches"
    );
}
