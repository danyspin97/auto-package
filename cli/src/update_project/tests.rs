use super::*;

/// A state root where `python-foo` has a risk flag of its last run and
/// `python-bar` has none.
fn state_with_a_risk() -> tempfile::TempDir {
    use ap_build::pipeline::PkgState;
    use ap_build::risk_flags::{RiskFlag, settle};
    use ap_build::workspace::Workspace;
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| {
        settle(
            st,
            "r1",
            vec![RiskFlag::ScriptDependency {
                added: true,
                line: "Requires(post): helper".into(),
            }],
        )
    })
    .unwrap();
    let bar = Workspace::open(root.path(), "python-bar").unwrap();
    PkgState::update(&bar, |_| {}).unwrap();
    root
}

fn pkg_item(pkg: &str) -> Item {
    Item {
        pkg: pkg.into(),
        origin: "Base:System".into(),
        ecosystem: ap_build::update::Ecosystem::GitRepo {
            url: "https://github.com/a/b".into(),
        },
        old_version: "1".into(),
        new_version: "2".into(),
        upstream_tag: None,
        dependency_of: None,
        findings: Vec::new(),
    }
}

#[test]
fn yes_refuses_a_batch_with_a_package_that_needs_a_review() {
    let root = state_with_a_risk();
    let gate = review_gate(root.path(), &["python-bar", "python-foo"]);
    assert_eq!(
        gate.needing,
        vec![(
            "python-foo".to_string(),
            vec!["RISK: added dependency line: Requires(post): helper".to_string()]
        )]
    );
    let lines = yes_refusal(true, &gate).expect("refused");
    assert!(lines[0].starts_with("--yes refuses to submit: 1 package(s) need a review"));
    assert!(lines[0].ends_with("run without --yes to review them"));
    assert_eq!(lines[1], "  python-foo");
    assert!(lines[2].contains("Requires(post): helper"));
    assert!(!lines.iter().any(|l| l.contains("python-bar")));
}

#[test]
fn clean_packages_and_an_interactive_run_are_not_refused() {
    let root = state_with_a_risk();
    // Only the clean package: nothing needs a review, even with --yes.
    let clean = review_gate(root.path(), &["python-bar"]);
    assert_eq!(yes_refusal(true, &clean), None);
    // Without --yes the confirmation asks instead of refusing.
    let flagged = review_gate(root.path(), &["python-foo"]);
    assert_eq!(yes_refusal(false, &flagged), None);
    // A package with no state at all cannot be read: --yes does not go ahead.
    let unknown = review_gate(root.path(), &["python-new"]);
    assert_eq!(unknown.unreadable.len(), 1, "{unknown:?}");
    assert!(yes_refusal(true, &unknown).is_some());
    assert_eq!(yes_refusal(false, &unknown), None);
}

#[test]
fn a_corrupt_state_refuses_under_yes_and_a_renamed_one_is_found_by_its_obs_name() {
    use ap_build::pipeline::PkgState;
    use ap_build::risk_flags::RiskFlag;
    use ap_build::workspace::Workspace;
    let root = state_with_a_risk();
    // The workspace is named for the RPM name; the OBS name is the other spelling.
    let ws = Workspace::open(root.path(), "python-apscheduler").unwrap();
    PkgState::update(&ws, |st| {
        st.obs_name = Some("python-APScheduler".into());
        ap_build::risk_flags::settle(
            st,
            "r1",
            vec![RiskFlag::NewScriptlet {
                header: "%post".into(),
            }],
        )
    })
    .unwrap();
    let gate = review_gate(root.path(), &["python-APScheduler"]);
    assert_eq!(gate.needing.len(), 1, "{gate:?}");
    assert!(gate.unreadable.is_empty());
    std::fs::write(ws.dir().join("state.json"), "{ not json").unwrap();
    let gate = review_gate(root.path(), &["python-APScheduler"]);
    assert!(gate.needing.is_empty());
    assert_eq!(gate.unreadable.len(), 1, "{gate:?}");
    assert!(yes_refusal(true, &gate).is_some());
}

#[test]
fn the_tui_refuses_a_flagged_request_under_yes_before_asking_obs() {
    use tui::PackageBackend as _;
    let root = state_with_a_risk();
    let backend = |yes: bool| SubmitBackend {
        branch: "home:me:branches:Base".into(),
        distro: None,
        arch: "x86_64".into(),
        state_root: root.path().to_path_buf(),
        yes,
        allow_arch: false,
        arch_checked: Default::default(),
        reporter: Arc::new(ap_core::sink::MemorySink::new()),
        items: Default::default(),
        created: Default::default(),
    };
    let flagged = backend(true);
    flagged.register(&pkg_item("python-foo"));
    let err = flagged.submit("python-foo").unwrap_err();
    assert!(
        matches!(&err, tui::SubmitFailure::Other(m)
                if m.contains("--yes refuses") && m.contains("run without --yes")),
        "{err:?}"
    );
    // Without --yes the request is not refused here (and this test makes
    // no OBS call, so the check is what the refusal is tested against).
    let asked = backend(false);
    asked.register(&pkg_item("python-foo"));
    assert_eq!(review_gate(root.path(), &["python-foo"]).needing.len(), 1);
    assert_eq!(
        yes_refusal(asked.yes, &review_gate(root.path(), &["python-foo"])),
        None
    );
}

#[test]
fn a_refused_request_reaches_the_tui_as_a_reason_or_an_open_request() {
    assert_eq!(
        submit_failure(SrOutcome::Existing("7".into())),
        tui::SubmitFailure::Existing("7".into())
    );
    assert_eq!(
        submit_failure(SrOutcome::NotEligible("it failed".into())),
        tui::SubmitFailure::Other("not eligible: it failed".into())
    );
}

#[test]
fn the_tui_backend_only_knows_the_packages_it_was_given() {
    use tui::PackageBackend as _;
    let b = SubmitBackend {
        branch: "home:me:branches:Base".into(),
        distro: None,
        arch: "x86_64".into(),
        state_root: PathBuf::from("/nonexistent-state-root"),
        yes: false,
        allow_arch: false,
        arch_checked: Default::default(),
        reporter: Arc::new(ap_core::sink::MemorySink::new()),
        items: Default::default(),
        created: Default::default(),
    };
    assert!(b.diff("foo").unwrap_err().contains("not a package"));
    assert!(b.preview("foo").is_err());
    assert!(b.submit("foo").is_err());
    assert!(b.created_lines().is_empty());
}
use clap::Parser;

#[derive(Parser)]
struct Wrapper {
    #[command(flatten)]
    args: UpdateProjectArgs,
}

fn args(extra: &[&str]) -> UpdateProjectArgs {
    let mut argv = vec!["x", "Base:System"];
    argv.extend_from_slice(extra);
    Wrapper::try_parse_from(argv).unwrap().args
}

#[test]
fn only_and_exclude_are_comma_lists_matched_by_fuzzy_name() {
    let a = args(&["--only", "python-a,Python_B", "--exclude", "python-b"]);
    assert!(wanted_by(&a.only, &a.exclude, "python-a"));
    assert!(
        !wanted_by(&a.only, &a.exclude, "python-b"),
        "excluded wins over only"
    );
    assert!(!wanted_by(&a.only, &a.exclude, "python-c"));
    // A multibuild flavor is judged by its source package.
    assert!(wanted_by(&a.only, &a.exclude, "python-a:python-a-extras"));
    assert!(wanted_by(&[], &[], "anything"));
    assert!(!wanted_by(&[], &["x".to_string()], "X"));
}

#[test]
fn nothing_is_submitted_or_changed_unless_asked() {
    let a = args(&[]);
    assert!(!a.submit && !a.yes && !a.dry_run && !a.update_deps);
    assert_eq!(a.branch_project, None);
}

#[test]
fn maintenance_takes_a_list_or_nothing_and_bugs_repeat() {
    let a = args(&[]);
    assert_eq!(a.maintenance, None);
    assert!(!a.maintenance_list && !a.no_bug_ref && !a.maintenance_source && a.bug.is_empty());
    let a = args(&["--maintenance", "--only", "curl"]);
    assert_eq!(a.maintenance, Some(vec![]));
    assert_eq!(a.only, vec!["curl"]);
    let a = args(&[
        "--maintenance",
        "SUSE:SLE-15-SP7:Update,SUSE:SLE-15-SP6:Update",
        "--bug",
        "bsc#1234567",
        "--bug",
        "CVE-2025-1234,jsc#SLE-1",
    ]);
    assert_eq!(
        a.maintenance,
        Some(vec![
            "SUSE:SLE-15-SP7:Update".into(),
            "SUSE:SLE-15-SP6:Update".into()
        ])
    );
    assert_eq!(a.bug, vec!["bsc#1234567", "CVE-2025-1234", "jsc#SLE-1"]);
}

#[test]
fn backport_flags_repeat_without_splitting_urls_and_need_maintenance() {
    let a = args(&[]);
    assert!(a.maintenance_mode.is_none() && a.backport.is_empty() && a.backport_job.is_none());
    let a = args(&[
        "--maintenance",
        "all",
        "--maintenance-mode",
        "update",
        "--backport",
        "https://x.org/a,b.patch",
        "--backport",
        "abc1234",
        "--backport-repo",
        "https://github.com/o/r",
        "--affected-by",
        "/usr/bin/check-affected",
    ]);
    assert_eq!(a.maintenance_mode.as_deref(), Some("update"));
    assert_eq!(a.backport, vec!["https://x.org/a,b.patch", "abc1234"]);
    assert_eq!(a.backport_repo.as_deref(), Some("https://github.com/o/r"));
    assert_eq!(a.affected_by.as_deref(), Some("/usr/bin/check-affected"));
    let mut summary = None;
    let err = run(args(&["--backport", "fix.patch"]), &mut summary).unwrap_err();
    assert!(
        err.to_string().contains("are for --maintenance runs"),
        "{err}"
    );
}

#[test]
fn a_backport_item_keeps_the_packaged_version() {
    let item = |decision| {
        backport_item(
            "SUSE:SLE-15-SP7:Update",
            &update::Candidate {
                rpm_name: "curl".into(),
                decision,
            },
        )
    };
    let eco = Ecosystem::GitRepo {
        url: "https://github.com/curl/curl".into(),
    };
    let got = item(update::Decision::Update {
        ecosystem: eco.clone(),
        old_version: "8.6.0".into(),
        new_version: "8.11.0".into(),
        upstream_tag: None,
    })
    .unwrap();
    assert_eq!(
        (got.old_version.as_str(), got.new_version.as_str()),
        ("8.6.0", "8.6.0")
    );
    assert_eq!(got.origin, "SUSE:SLE-15-SP7:Update");
    let got = item(update::Decision::UpToDate {
        ecosystem: eco,
        version: "8.6.0".into(),
    })
    .unwrap();
    assert_eq!(got.new_version, "8.6.0");
    let err = item(update::Decision::Skipped(update::SkipReason::NoSourceLine)).unwrap_err();
    assert!(err.contains("cannot be rebuilt for the backport"), "{err}");
}

#[test]
fn improving_in_place_is_off_unless_asked_and_set_by_the_command() {
    let a = args(&[]);
    assert!(!a.in_place && !a.improve, "improve is never a flag");
    assert!(args(&["--in-place"]).in_place);
}

#[test]
fn the_repository_is_the_projects_own_unless_distro_names_one() {
    assert_eq!(args(&[]).distro, None);
    assert_eq!(
        args(&["--distro", "Leap_16"]).distro.as_deref(),
        Some("Leap_16")
    );
}

#[test]
fn refresh_is_off_unless_asked() {
    assert!(!args(&[]).refresh);
    assert!(args(&["--refresh"]).refresh);
}

#[test]
fn check_is_another_name_for_dry_run() {
    assert!(args(&["--check"]).dry_run);
    assert!(args(&["--dry-run"]).dry_run);
    assert!(!args(&[]).unsafe_resolve);
    assert!(args(&["--unsafe-resolve"]).unsafe_resolve);
}

#[test]
fn the_update_command_names_its_source_with_project_and_has_no_update_project() {
    use clap::Parser as _;
    let parse = |argv: &[&str]| crate::Cli::try_parse_from(argv);
    let Ok(cli) = parse(&["ap", "update", "--project", "Base:System", "--in-place"]) else {
        panic!("update --project --in-place must parse");
    };
    let crate::Commands::Update(a) = cli.command else {
        panic!("not update");
    };
    assert_eq!((a.source.as_str(), a.in_place), ("Base:System", true));
    assert!(parse(&["ap", "update-project", "Base:System"]).is_err());
}

#[test]
fn the_agent_is_told_the_findings_and_to_keep_the_version() {
    let mut item = Item {
        pkg: "libfoo".into(),
        origin: "Base:System".into(),
        ecosystem: Ecosystem::GitRepo {
            url: "https://github.com/a/b".into(),
        },
        old_version: "1.0".into(),
        new_version: "1.0".into(),
        upstream_tag: None,
        dependency_of: None,
        findings: Vec::new(),
    };
    assert_eq!(improve_problem(&item), None, "an update has no findings");
    item.findings = vec!["rpmlint-error: devel-file-in-non-devel-package x1".into()];
    let p = improve_problem(&item).unwrap();
    assert!(p.contains("libfoo 1.0"));
    assert!(p.contains("- rpmlint-error: devel-file-in-non-devel-package x1"));
    assert!(p.contains("keep the version"));
}

#[test]
fn min_free_gb_defaults_to_three_and_zero_disables() {
    assert_eq!(args(&[]).min_free_gb, 3);
    assert_eq!(args(&["--min-free-gb", "0"]).min_free_gb, 0);
}

#[test]
fn nvidia_redist_is_off_unless_asked() {
    assert!(!args(&[]).nvidia_redist && args(&[]).nvidia.is_none());
    assert!(args(&["--nvidia-redist"]).nvidia_redist);
    assert_eq!(
        args(&["--nvidia", "redist"]).nvidia,
        Some(crate::NvidiaMode::Redist)
    );
}

#[test]
fn branch_call_owns_only_packages_missing_before_it() {
    let before: Result<Arc<HashSet<String>>, String> =
        Ok(Arc::new(["foo".to_string()].into_iter().collect()));
    assert!(created_by_branch(&before, "bar"));
    assert!(!created_by_branch(&before, "foo"));
    let unknown: Result<Arc<HashSet<String>>, String> = Err("no listing".to_string());
    assert!(!created_by_branch(&unknown, "bar"));
}

#[test]
fn the_arches_flag_is_validated_and_the_rows_become_events() {
    use super::arch;
    use ap_build::arch_check::{ArchRow, ArchState};
    let mut a = args(&[]);
    assert_eq!(arch::wanted_arches(&a).unwrap(), None);
    a.arches = vec!["s390x".into(), "s390x".into(), "ppc64le".into()];
    assert_eq!(
        arch::wanted_arches(&a).unwrap(),
        Some(vec!["s390x".to_string(), "ppc64le".to_string()])
    );
    a.arches = vec!["mips".into()];
    assert!(arch::wanted_arches(&a).unwrap_err().contains("mips"));
    let row = ArchRow {
        pkg: "p".into(),
        repo: "ports".into(),
        arch: "s390x".into(),
        state: ArchState::Failed {
            detail: Some("boom".into()),
        },
        baseline_fails: true,
        log: Some("/w/p.log".into()),
    };
    let ev = arch::event_result(&row);
    assert_eq!(ev.state, "failed");
    assert_eq!(ev.detail.as_deref(), Some("boom"));
    assert!(ev.origin_fails);
}

#[test]
fn the_ports_follow_the_flag_and_the_arches_named() {
    use super::arch;
    let mut a = args(&[]);
    assert!(arch::ports_for(&a, None).is_empty());
    a.ports = true;
    assert_eq!(arch::ports_for(&a, None).len(), 3);
    a.ports = false;
    let s390x = vec!["s390x".to_string()];
    assert_eq!(arch::ports_for(&a, Some(&s390x)).len(), 1);
    let x86 = vec!["x86_64".to_string()];
    assert!(arch::ports_for(&a, Some(&x86)).is_empty());
    a.arches = vec!["armv7l".into()];
    let err = arch::wanted_arches(&a).unwrap_err();
    assert!(err.contains("no Tumbleweed armv7l in OBS"), "{err}");
}
