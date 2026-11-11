//! The self-check's tests.

use super::*;

fn write(state_root: &Path, name: &str, f: impl FnOnce(&mut PkgState)) {
    let ws = Workspace::open(state_root, name).unwrap();
    PkgState::update(&ws, f).unwrap();
}

#[test]
fn the_self_check_finds_what_the_parts_disagree_about() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    std::fs::create_dir_all(checkouts.join("python-a")).unwrap();
    std::fs::write(
        checkouts.join("python-a/python-a.spec"),
        "Name: python-a\nVersion: 2.0\nSource0: https://h/a-2.0-py3-none-any.whl\n",
    )
    .unwrap();
    std::fs::write(checkouts.join("python-a/stray.bin"), "x").unwrap();
    std::fs::write(checkouts.join("python-a/wheel-origin.txt"), "x").unwrap();
    write(&state_root, "python-a", |s| {
        s.last_outcome = Some("success".into());
        s.last_built_version = Some("1.0".into());
    });
    write(&state_root, "python-b", |s| {
        s.last_outcome = Some("failed".into())
    });
    let statuses = HashMap::from([
        ("python-a".to_string(), BuildStatus::Failed),
        ("Python-B".to_string(), BuildStatus::Succeeded),
    ]);
    let found = run(&state_root, &checkouts, || Some(statuses), |_| false);
    let all = found.join("\n");
    assert!(
        all.contains("not named by the spec") && all.contains("stray.bin"),
        "{all}"
    );
    assert!(
        all.contains("state.json records a successful build of 1.0"),
        "{all}"
    );
    assert!(
        all.contains("the spec was switched to an upstream wheel"),
        "{all}"
    );
    assert!(
        all.contains("state.json says it built, but OBS shows Failed"),
        "{all}"
    );
    assert!(
        all.contains("state.json says failed, but OBS shows it built"),
        "{all}"
    );
}

#[test]
fn the_checkout_is_looked_up_under_the_obs_name_the_state_records() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    // The workspace is keyed by the rpm name; the checkout by the OBS name.
    std::fs::create_dir_all(checkouts.join("python-APScheduler")).unwrap();
    std::fs::write(
        checkouts.join("python-APScheduler/python-APScheduler.spec"),
        "Name: python-APScheduler\nVersion: 2.0\n",
    )
    .unwrap();
    std::fs::write(checkouts.join("python-APScheduler/stray.bin"), "x").unwrap();
    write(&state_root, "python-apscheduler", |s| {
        s.obs_name = Some("python-APScheduler".into());
        s.last_outcome = Some("success".into());
        s.last_built_version = Some("1.0".into());
    });
    let all = run(&state_root, &checkouts, || None, |_| false).join("\n");
    assert!(
        all.contains("state.json records a successful build of 1.0 but the spec says 2.0"),
        "{all}"
    );
    assert!(all.contains("stray.bin not named"), "{all}");
    assert!(!all.contains("the next commit"), "{all}");
    // Without obs_name the rpm name is the checkout: nothing is there to find.
    write(&state_root, "python-apscheduler", |s| s.obs_name = None);
    let all = run(&state_root, &checkouts, || None, |_| false).join("\n");
    assert!(!all.contains("python-apscheduler"), "{all}");
}

#[test]
fn the_tools_own_files_in_a_checkout_are_not_findings() {
    assert!(is_tool_bookkeeping("series"));
    assert!(is_tool_bookkeeping("state.json"));
    assert!(is_tool_bookkeeping("foo.spec.pre-wheel-switch"));
    assert!(!is_tool_bookkeeping("foo.spec"));
    assert!(!is_tool_bookkeeping("serieses"));
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    let dir = checkouts.join("python-q");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("python-q.spec"), "Name: python-q\nVersion: 1.0\n").unwrap();
    for f in ["series", "state.json", "stray.bin"] {
        std::fs::write(dir.join(f), "x").unwrap();
    }
    write(&state_root, "python-q", |s| {
        s.last_outcome = Some("failed".into())
    });
    let found = run(&state_root, &checkouts, || None, |_| false).join("\n");
    assert!(found.contains("stray.bin not named"), "{found}");
    assert!(!found.contains("series"), "{found}");
}

#[test]
fn a_stale_archive_and_an_unreadable_state_file_are_found() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    std::fs::create_dir_all(checkouts.join("python-a")).unwrap();
    std::fs::write(
        checkouts.join("python-a/python-a.spec"),
        "Name: python-a\nVersion: 2.0\nSource0: a-2.0.tar.gz\n",
    )
    .unwrap();
    std::fs::write(checkouts.join("python-a/a-1.0.tar.gz"), "x").unwrap();
    write(&state_root, "python-a", |s| {
        s.last_outcome = Some("failed".into())
    });
    std::fs::create_dir_all(state_root.join("python-b")).unwrap();
    std::fs::write(state_root.join("python-b/state.json"), "{not json").unwrap();
    let all = run(&state_root, &checkouts, || None, |_| false).join("\n");
    assert!(
        all.contains("the spec says 2.0 but the checkout's source archive(s) are a-1.0.tar.gz"),
        "{all}"
    );
    assert!(all.contains("state.json cannot be read"), "{all}");
    // The matching archive is no finding.
    std::fs::write(checkouts.join("python-a/a-2.0.tar.gz"), "x").unwrap();
    assert!(
        !run(&state_root, &checkouts, || None, |_| false)
            .join("\n")
            .contains("the spec says")
    );
}

#[test]
fn names_version_needs_a_separator_before_and_an_archive_after() {
    assert!(names_version("foo-1.0.tar.gz", "1.0"));
    assert!(names_version("foo_1.0.tgz", "1.0"));
    assert!(names_version("foo-1.0-py3-none-any.whl", "1.0"));
    assert!(names_version("foo-1.0-cp311-cp311-linux_x86_64.whl", "1.0"));
    // A digit before the match means it is part of another version.
    assert!(!names_version("a-11.0.tar.gz", "1.0"));
    assert!(!names_version("a-21.0.tar.gz", "1.0"));
    // Another archive's suffix is not this version's main archive.
    assert!(!names_version("foo-1.0-docs.tar.gz", "1.0"));
    assert!(!names_version("foo-1.0.post1.tar.gz", "1.0"));
}

#[test]
fn names_version_takes_a_wheel_tag_but_no_other_suffix_on_an_archive() {
    assert!(names_version("foo-1.0-py3-none-any.whl", "1.0"));
    assert!(names_version(
        "foo-1.0-pp310-pypy310_pp73-linux_x86_64.whl",
        "1.0"
    ));
    // A companion archive whose name carries a suffix is not the main one.
    assert!(!names_version("foo-1.0-cpp.tar.gz", "1.0"));
    assert!(!names_version("foo-1.0-python.tar.gz", "1.0"));
    assert!(!names_version("foo-1.0-py3.tar.gz", "1.0"));
    assert!(!names_version("foo-1.0-cp311.zip", "1.0"));
    assert!(!names_version("foo-1.0-py3-none-any.whl.bak", "1.0"));
}

#[test]
fn archive_names_match_the_spec_version_and_wheel_leftovers_are_not_noise() {
    assert!(names_version("a-1.0.tar.gz", "1.0"));
    assert!(names_version(
        "libcublas-linux-x86_64-13.8.0.4-archive.tar.xz",
        "13.8.0.4"
    ));
    assert!(names_version(
        "cudnn-linux-x86_64-9.27.0.42_cuda13-archive.tar.xz",
        "9.27.0.42"
    ));
    assert!(names_version("v1.0.zip", "1.0"));
    assert!(!names_version("a-1.0.1.tar.gz", "1.0"));
    assert!(!names_version("a-11.0.tar.gz", "1.0.1"));
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    let dir = checkouts.join("python-w");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("python-w.spec"),
        "Name: python-w\nVersion: 1.0\nSource0: w-1.0-py3-none-any.whl\n",
    )
    .unwrap();
    for f in [
        "w-1.0.tar.gz",
        "wheel-origin.txt",
        "python-w.spec.pre-wheel-switch",
        "stray.bin",
    ] {
        std::fs::write(dir.join(f), "x").unwrap();
    }
    write(&state_root, "python-w", |s| {
        s.last_outcome = Some("failed".into())
    });
    let found = run(&state_root, &checkouts, || None, |_| false).join("\n");
    // One finding for the leftovers a wheel switch makes, one for the stray file.
    assert!(found.contains("stray.bin not named"), "{found}");
    assert!(
        !found.contains("wheel-origin.txt") || found.contains("wheel-origin.txt is missing"),
        "{found}"
    );
    assert!(!found.contains("w-1.0.tar.gz"), "{found}");
}

#[test]
fn an_archive_the_spec_names_under_another_spelling_of_its_version_is_not_stale() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    let cases = [
        // `src_version` keeps the `-` that `Version:` writes as `.`.
        (
            "dmraid",
            "Name: dmraid\nVersion: 1.0.0.rc16.3\n%define src_version 1.0.0.rc16-3\n\
             Source: https://h/dmraid-%{src_version}.tar.bz2\n",
            "dmraid-1.0.0.rc16-3.tar.bz2",
        ),
        // A `-release` suffix written into `Source0`.
        (
            "popt",
            "Name: popt\nVersion: 1.19\nSource0: %{name}-%{version}-release.tar.gz\n",
            "popt-1.19-release.tar.gz",
        ),
        // An archive of another version than the spec's is still stale.
        (
            "elfutils",
            "Name: elfutils\nVersion: 0.197\nSource: https://h/%{name}-%{version}.tar.bz2\n",
            "elfutils-0.196.tar.bz2",
        ),
    ];
    for (name, spec, archive) in cases {
        std::fs::create_dir_all(checkouts.join(name)).unwrap();
        std::fs::write(checkouts.join(format!("{name}/{name}.spec")), spec).unwrap();
        std::fs::write(checkouts.join(format!("{name}/{archive}")), "x").unwrap();
        write(&state_root, name, |s| {
            s.last_outcome = Some("failed".into());
        });
    }
    let all = run(&state_root, &checkouts, || None, |_| false).join("\n");
    assert!(!all.contains("dmraid: the spec says"), "{all}");
    assert!(!all.contains("popt: the spec says"), "{all}");
    assert!(
        all.contains(
            "the spec says 0.197 but the checkout's source archive(s) are elfutils-0.196.tar.bz2"
        ),
        "{all}"
    );
}

#[test]
fn an_unknown_obs_status_skips_the_obs_comparison_and_a_clean_package_has_no_findings() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    std::fs::create_dir_all(checkouts.join("python-a")).unwrap();
    std::fs::write(
        checkouts.join("python-a/python-a.spec"),
        "Name: python-a\nVersion: 1.0\n",
    )
    .unwrap();
    write(&state_root, "python-a", |s| {
        s.last_outcome = Some("success".into());
        s.last_built_version = Some("1.0".into());
    });
    assert!(run(&state_root, &checkouts, || None, |_| false).is_empty());
    let ok = HashMap::from([("python-a".to_string(), BuildStatus::Succeeded)]);
    assert!(run(&state_root, &checkouts, || Some(ok), |_| false).is_empty());
}

#[test]
fn a_skip_that_obs_built_is_a_disagreement() {
    let state = PkgState {
        last_outcome: Some("skipped".into()),
        ..Default::default()
    };
    let never = || panic!("a skip is decided without the checkout");
    assert!(check_obs("pkg", &state, Some(&BuildStatus::Succeeded), None, never).is_some());
    assert!(check_obs("pkg", &state, Some(&BuildStatus::Excluded), None, never).is_none());
}

#[test]
fn an_obs_build_the_reconcile_declines_says_why() {
    let failed = PkgState {
        last_outcome: Some("failed".into()),
        last_built_version: Some("1.0".into()),
        ..Default::default()
    };
    let built = Some(&BuildStatus::Succeeded);
    let other = check_obs("pkg", &failed, built, Some("2.0"), || false).unwrap();
    assert!(
        other.starts_with(
            "pkg: state.json says failed, but OBS shows it built and its RPMs are not taken:"
        ) && other.contains("the spec says 2.0")
            && other.contains("run update for it"),
        "{other}"
    );
    let dirty = check_obs("pkg", &failed, built, Some("1.0"), || true).unwrap();
    assert!(dirty.contains("discard or commit them"), "{dirty}");
    // A clean checkout at the built version is what the reconcile takes: no finding.
    assert!(check_obs("pkg", &failed, built, Some("1.0"), || false).is_none());
}

#[test]
fn a_stale_review_is_no_finding_because_start_up_drops_it() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    std::fs::create_dir_all(checkouts.join("python-g")).unwrap();
    std::fs::write(
        checkouts.join("python-g/python-g.spec"),
        "Name: python-g\nVersion: 0.7.11\n",
    )
    .unwrap();
    write(&state_root, "python-g", |s| {
        s.reviewed_version = Some("0.7.12".into());
        s.last_outcome = Some("success".into());
        s.last_built_version = Some("0.7.11".into());
    });
    assert!(run(&state_root, &checkouts, || None, |_| false).is_empty());
}

#[test]
fn an_old_archive_obs_holds_is_no_finding_and_a_tracked_file_the_spec_does_not_name_is() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("proj");
    let dir = checkouts.join("python-t");
    std::fs::create_dir_all(dir.join(".osc")).unwrap();
    std::fs::write(
        dir.join("python-t.spec"),
        "Name: python-t\nVersion: 2.0\nSource0: t-%{version}.tar.gz\n",
    )
    .unwrap();
    std::fs::write(dir.join("t-2.0.tar.gz"), "x").unwrap();
    std::fs::write(dir.join("t-1.0.tar.gz"), "x").unwrap();
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    std::fs::write(
        dir.join(".osc/_files"),
        "<directory name=\"python-t\">\
         <entry name=\"t-1.0.tar.gz\" md5=\"x\" size=\"1\" mtime=\"1\" />\
         <entry name=\"notes.txt\" md5=\"x\" size=\"1\" mtime=\"1\" />\
         </directory>",
    )
    .unwrap();
    write(&state_root, "python-t", |s| {
        s.last_outcome = Some("failed".into())
    });
    let found = run(&state_root, &checkouts, || None, |_| false).join("\n");
    assert!(!found.contains("t-1.0.tar.gz"), "{found}");
    assert!(
        found.contains("OBS holds notes.txt, which the spec does not name"),
        "{found}"
    );
    assert!(!found.contains("not named by the spec"), "{found}");
}

#[test]
fn the_self_check_lists_source_archives_by_the_one_list() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "foo-1.0.zip",
        "foo-1.0-py3-none-any.whl",
        "foo-1.0.tar",
        "vendor.tar.zst",
        "registry.tar.zst",
        "foo.spec",
    ] {
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    assert_eq!(
        source_archives(dir.path()),
        ["foo-1.0-py3-none-any.whl", "foo-1.0.zip"]
    );
}
