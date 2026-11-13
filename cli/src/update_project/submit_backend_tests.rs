use super::tests::{pkg_item, state_with_a_risk};
use super::*;

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
        include_ports: false,
        arch_checked: Default::default(),
        ready: ready::ReadyCheck::Skip,
        arch_deadline: Arc::new(ArchDeadline::new(osc::OVERALL_MAX_WAIT)),
        untied_wait: Default::default(),
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
        include_ports: false,
        arch_checked: Default::default(),
        ready: ready::ReadyCheck::Skip,
        arch_deadline: Arc::new(ArchDeadline::new(osc::OVERALL_MAX_WAIT)),
        untied_wait: Default::default(),
        reporter: Arc::new(ap_core::sink::MemorySink::new()),
        items: Default::default(),
        created: Default::default(),
    };
    assert!(b.diff("foo").unwrap_err().contains("not a package"));
    assert!(b.preview("foo").is_err());
    assert!(b.submit("foo").is_err());
    assert!(b.created_lines().is_empty());
}
