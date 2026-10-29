
#[test]
fn the_summary_lists_only_packages_with_recorded_flags() {
    use ap_build::fix_flags::{FixFlag, settle};
    use ap_build::pipeline::PkgState;
    use ap_build::workspace::Workspace;
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| settle(st, "r1", vec![FixFlag::CheckRemoved])).unwrap();
    Workspace::open(root.path(), "python-bar").unwrap();
    let pkgs = ["python-foo".to_string(), "python-bar".to_string()];
    assert_eq!(
        fix_flagged(root.path(), pkgs.iter()),
        vec![("python-foo".to_string(), vec![FixFlag::CheckRemoved.line()])]
    );
}

#[test]
fn the_summary_lists_the_risks_of_the_last_run_only_for_packages_with_some() {
    use ap_build::pipeline::PkgState;
    use ap_build::risk_flags::{RiskFlag, settle};
    use ap_build::workspace::Workspace;
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| {
        settle(
            st,
            "r1",
            vec![RiskFlag::NewScriptlet {
                header: "%post".into(),
            }],
        )
    })
    .unwrap();
    Workspace::open(root.path(), "python-bar").unwrap();
    let pkgs = ["python-foo".to_string(), "python-bar".to_string()];
    let risks = super::with_lines(pkgs.iter(), |pkg| {
        ap_build::risk_flags::lines_of_package(root.path(), pkg)
    });
    assert_eq!(
        risks,
        vec![(
            "python-foo".to_string(),
            vec!["RISK: new scriptlet: %post".to_string()]
        )]
    );
}

#[test]
fn the_plan_counts_each_kind_of_package_as_it_is_added() {
    use ap_build::update::{Candidate, Decision, Ecosystem};
    let sink = ap_core::sink::MemorySink::new();
    let mut plan = super::PlanReport::default();
    let eco = || Ecosystem::Pypi {
        pypi_name: "x".into(),
    };
    for decision in [
        Decision::Update {
            ecosystem: eco(),
            old_version: "1".into(),
            new_version: "2".into(),
        },
        Decision::UpToDate {
            ecosystem: eco(),
            version: "1".into(),
        },
    ] {
        plan.add(
            &Candidate {
                rpm_name: "python-x".into(),
                decision,
            },
            &sink,
        );
    }
    assert_eq!((plan.n_update, plan.n_uptodate, plan.total()), (1, 1, 2));
}

#[test]
fn a_settled_tui_run_exits_zero_only_without_errors() {
    assert!(super::tui_run_exit(true, None, Ok(()), Vec::new()).is_ok());
}

#[test]
fn a_settled_tui_run_with_a_standing_failure_exits_with_it() {
    // A request that failed before it had a row: no retry can fix it.
    let err = super::tui_run_exit(
        true,
        None,
        Ok(()),
        vec!["bad name: invalid package name".to_string()],
    )
    .unwrap_err();
    assert_eq!(format!("{err}"), "bad name: invalid package name");
}

#[test]
fn a_settled_tui_run_whose_root_failed_exits_with_its_error() {
    let err = super::tui_run_exit(true, None, Err(color_eyre::eyre::eyre!("root")), Vec::new())
        .unwrap_err();
    assert_eq!(format!("{err}"), "root");
}

#[test]
fn a_tui_that_died_mid_run_is_judged_by_its_builds() {
    let dead = || Some("render failed".to_string());
    // Every build succeeded: the run is a success, not an unsettled TUI run.
    assert!(super::tui_run_exit(false, dead(), Ok(()), Vec::new()).is_ok());
    // The root or an added package failed: that is the exit.
    let err = super::tui_run_exit(false, dead(), Ok(()), vec!["a: first".to_string()]).unwrap_err();
    assert_eq!(format!("{err}"), "a: first");
    let err = super::tui_run_exit(
        false,
        dead(),
        Err(color_eyre::eyre::eyre!("root failed")),
        Vec::new(),
    )
    .unwrap_err();
    assert_eq!(format!("{err}"), "root failed");
}

#[test]
fn an_unsettled_tui_run_exits_with_its_own_error_first() {
    let err = super::tui_run_exit(
        false,
        None,
        Err(color_eyre::eyre::eyre!("root failed")),
        vec!["added failed".to_string()],
    )
    .unwrap_err();
    assert_eq!(format!("{err}"), "root failed");

    let err = super::tui_run_exit(
        false,
        None,
        Ok(()),
        vec!["a: first".to_string(), "b: second".to_string()],
    )
    .unwrap_err();
    assert_eq!(format!("{err}"), "a: first; b: second");
}

#[test]
fn an_unsettled_tui_run_without_errors_says_it_ended_early() {
    let err = super::tui_run_exit(false, None, Ok(()), Vec::new()).unwrap_err();
    assert_eq!(
        format!("{err}"),
        "the run ended before every package was built"
    );
}

#[test]
fn a_condition_failure_is_queued_again_in_the_same_run() {
    use super::{FailureKind as K, dep_failure_retries as r};
    // Even with the rounds spent: the condition counters are the bound.
    assert!(r(K::ConditionUnmet, 5, 1));
    assert!(r(K::Other, 0, 2));
    assert!(!r(K::Other, 2, 2));
    assert!(!r(K::Exhausted, 0, 3));
    assert!(!r(K::RateLimited, 0, 3));
}

use super::*;

#[test]
fn unsafe_resolve_is_off_unless_asked_for_on_build_and_update() {
    let unsafe_resolve = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
        Commands::Build { unsafe_resolve, .. } => unsafe_resolve,
        Commands::Update(a) => a.unsafe_resolve,
        _ => panic!("not build or update"),
    };
    let build = ["auto-package", "build", "httpx", "--project", "home:u"];
    let update = ["auto-package", "update", "--project", "home:u"];
    assert!(!unsafe_resolve(&build));
    assert!(!unsafe_resolve(&update));
    assert!(unsafe_resolve(
        &[&build[..], &["--unsafe-resolve"]].concat()
    ));
    assert!(unsafe_resolve(
        &[&update[..], &["--unsafe-resolve"]].concat()
    ));
}

#[test]
fn build_takes_min_free_gb_defaulting_to_three() {
    let min_free = |extra: &[&str]| {
        let base = ["auto-package", "build", "httpx", "--project", "home:u"];
        match Cli::try_parse_from([&base[..], extra].concat())
            .unwrap()
            .command
        {
            Commands::Build { min_free_gb, .. } => min_free_gb,
            _ => panic!("not build"),
        }
    };
    assert_eq!(min_free(&[]), 3);
    assert_eq!(min_free(&["--min-free-gb", "0"]), 0);
}

#[test]
fn build_takes_nvidia_mode_defaulting_to_none_chosen() {
    let parse = |extra: &[&str]| {
        let base = ["auto-package", "build", "httpx", "--project", "home:u"];
        Cli::try_parse_from([&base[..], extra].concat()).map(|c| match c.command {
            Commands::Build {
                nvidia,
                nvidia_redist,
                ..
            } => (nvidia, nvidia_redist),
            _ => panic!("not build"),
        })
    };
    assert_eq!(parse(&[]).unwrap(), (None, false));
    assert_eq!(
        parse(&["--nvidia", "redist"]).unwrap(),
        (Some(NvidiaMode::Redist), false)
    );
    assert_eq!(
        parse(&["--nvidia", "wheel"]).unwrap(),
        (Some(NvidiaMode::Wheel), false)
    );
    assert_eq!(parse(&["--nvidia-redist"]).unwrap(), (None, true));
    assert!(parse(&["--nvidia", "rpm"]).is_err());
    assert!(parse(&["--nvidia", "wheel", "--nvidia-redist"]).is_err());
}

#[test]
fn the_nvidia_mode_is_explicit_then_remembered_then_wheel() {
    use NvidiaMode::*;
    assert_eq!(resolve_nvidia_mode(None, None), (Wheel, None));
    assert_eq!(resolve_nvidia_mode(None, Some(Redist)), (Redist, None));
    assert_eq!(resolve_nvidia_mode(Some(Redist), None), (Redist, None));
    assert_eq!(
        resolve_nvidia_mode(Some(Redist), Some(Redist)),
        (Redist, None)
    );
    let (mode, warning) = resolve_nvidia_mode(Some(Wheel), Some(Redist));
    assert_eq!(mode, Wheel);
    assert!(warning.unwrap().contains("switching to wheel"));
}

#[test]
fn the_nvidia_mode_is_remembered_in_the_work_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_nvidia_mode(dir.path()), None);
    // A plain run in the default mode leaves nothing behind.
    write_nvidia_mode(dir.path(), NvidiaMode::Wheel, false).unwrap();
    assert!(!dir.path().join(NVIDIA_MODE_FILE).exists());
    write_nvidia_mode(dir.path(), NvidiaMode::Redist, true).unwrap();
    assert_eq!(read_nvidia_mode(dir.path()), Some(NvidiaMode::Redist));
    // Once there, it is kept up to date, including a switch back.
    write_nvidia_mode(dir.path(), NvidiaMode::Wheel, true).unwrap();
    assert_eq!(read_nvidia_mode(dir.path()), Some(NvidiaMode::Wheel));
    std::fs::write(dir.path().join(NVIDIA_MODE_FILE), "garbage").unwrap();
    assert_eq!(read_nvidia_mode(dir.path()), None);
}

#[test]
fn a_running_or_finished_package_is_not_started_again() {
    use ap_core::event::SelectionReason;
    let target = |name: &str| ap_build::pipeline::TargetInfo {
        name: name.to_string(),
        version: "1.0".to_string(),
        reason: SelectionReason::FirstUnbuiltDependency,
        blocked_by: vec![],
    };
    let targets = vec![target("alpha"), target("beta"), target("gamma")];
    let in_flight = vec![ap_build::osc::rpm_name("beta")];
    let done: std::collections::HashSet<String> =
        [ap_build::osc::rpm_name("gamma")].into_iter().collect();
    let fresh: Vec<String> = unstarted_targets(targets, &in_flight, &done)
        .into_iter()
        .map(|(t, _)| t.name)
        .collect();
    assert_eq!(fresh, vec!["alpha".to_string()]);
}

#[test]
fn a_package_is_run_under_its_lowercase_name_whatever_obs_lists() {
    use ap_core::event::SelectionReason;
    // OBS lists the package as `python-APScheduler`. The run key is the
    // lowercase guess, so the state directory, the TUI row and the
    // in-flight check all agree on one name.
    assert_eq!(run_name("APScheduler"), "python-apscheduler");
    assert_eq!(run_name("apscheduler"), "python-apscheduler");
    let target = || ap_build::pipeline::TargetInfo {
        name: "APScheduler".to_string(),
        version: "1.0".to_string(),
        reason: SelectionReason::FirstUnbuiltDependency,
        blocked_by: vec![],
    };
    let running = vec!["python-apscheduler".to_string()];
    let fresh = unstarted_targets(vec![target()], &running, &Default::default());
    assert!(fresh.is_empty(), "a running package is not started again");
    let fresh = unstarted_targets(vec![target()], &[], &Default::default());
    assert_eq!(fresh[0].1, "python-apscheduler");
    assert!(!fresh[0].1.contains("APScheduler"));
}

#[test]
fn repo_package_name_ignores_subpaths_and_queries() {
    for url in [
        "https://github.com/o/r",
        "https://github.com/o/r.git",
        "https://github.com/o/r/tree/main",
        "https://github.com/o/r/releases/tag/v1",
        "https://github.com/o/r?tab=readme",
        "https://github.com/o/r#readme",
    ] {
        assert_eq!(repo_package_name(url), "r", "{url}");
    }
}

#[test]
fn validate_package_name_rejects_unsafe_names() {
    assert!(validate_package_name("ripgrep").is_ok());
    for bad in ["", "-rf", "a/b", "a b", "a\nb", "a\tb"] {
        assert!(validate_package_name(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn permit_releases_on_panic() {
    let sem = make_semaphore(1);
    let s2 = sem.clone();
    let r = std::thread::spawn(move || {
        let _p = Permit::acquire(&s2);
        panic!("boom");
    })
    .join();
    assert!(r.is_err());
    // Would deadlock if the slot had leaked.
    let _p = Permit::acquire(&sem);
}

/// drogon-shaped case the user reported: the agent passed
/// `trantor-devel` through as the dep name instead of translating
/// to upstream `trantor`. The candidate list must strip the
/// `-devel` suffix so `find_source_repo_candidates` has a chance.
#[test]
fn upstream_name_candidates_strips_devel_suffix() {
    assert_eq!(
        upstream_name_candidates("trantor-devel"),
        vec!["trantor-devel".to_string(), "trantor".to_string()]
    );
}

#[test]
fn upstream_name_candidates_strips_lib_prefix_and_devel_suffix_together() {
    // The full RPM BuildRequires shape. Three candidates: original,
    // -devel stripped, both stripped.
    assert_eq!(
        upstream_name_candidates("libtrantor-devel"),
        vec![
            "libtrantor-devel".to_string(),
            "libtrantor".to_string(),
            "trantor".to_string(),
        ]
    );
}

#[test]
fn upstream_name_candidates_handles_hyphenated_project_names() {
    // yaml-cpp is drogon's other recurring BuildRequires. The
    // hyphen inside the name must survive — only the trailing
    // -devel is cruft.
    assert_eq!(
        upstream_name_candidates("yaml-cpp-devel"),
        vec!["yaml-cpp-devel".to_string(), "yaml-cpp".to_string()]
    );
}

#[test]
fn upstream_name_candidates_also_recognises_dev_and_headers() {
    for (dep, stripped) in [
        ("something-dev", "something"),
        ("whatever-headers", "whatever"),
    ] {
        let out = upstream_name_candidates(dep);
        assert!(out.contains(&stripped.to_string()), "{dep:?} → {out:?}");
    }
}

#[test]
fn upstream_name_candidates_leaves_a_canonical_name_alone() {
    // Nothing to strip — only one candidate, same as pre-fix
    // behavior for correctly-translated deps.
    assert_eq!(
        upstream_name_candidates("trantor"),
        vec!["trantor".to_string()]
    );
    assert_eq!(upstream_name_candidates("httpx"), vec!["httpx".to_string()]);
}

#[test]
fn upstream_name_candidates_does_not_strip_lib_from_a_project_whose_name_is_lib() {
    // Pathological: a project called just `lib` would strip to
    // empty. Guard against the empty-string candidate — we skip it,
    // keeping just the original.
    assert_eq!(upstream_name_candidates("lib"), vec!["lib".to_string()]);
}

#[test]
fn a_repo_url_is_recognised_in_the_name_position() {
    assert!(looks_like_url("https://github.com/BurntSushi/ripgrep"));
    assert!(looks_like_url("git@github.com:BurntSushi/ripgrep.git"));
    assert!(!looks_like_url("markupsafe"));
    assert!(!looks_like_url("nvidia-ml-py"));
}

#[test]
fn the_package_name_comes_from_the_last_path_segment() {
    for url in [
        "https://github.com/BurntSushi/ripgrep",
        "https://github.com/BurntSushi/ripgrep/",
        "https://github.com/BurntSushi/ripgrep.git",
        "git@github.com:BurntSushi/ripgrep.git",
    ] {
        assert_eq!(repo_package_name(url), "ripgrep", "{url}");
    }
    assert_eq!(
        repo_package_name("https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz"),
        "hello"
    );
    assert_eq!(
        repo_package_name("https://download.savannah.gnu.org/releases/lzip/"),
        "lzip"
    );
    assert_eq!(
        repo_package_name("https://gitlab.com/group/sub/thing"),
        "thing"
    );
}
