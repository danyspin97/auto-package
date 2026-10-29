use super::hygiene_tests::config;
use super::*;

/// A package whose OBS revision is built and recorded, with two flags a
/// commit of `r1` settled.
fn built_with_flags(root: &Path) {
    let ws = Workspace::open(root, "python-foo").unwrap();
    PkgState::update(&ws, |st| {
        st.last_built_version = Some("1.0".into());
        st.last_outcome = Some("success".into());
        crate::fix_flags::settle(st, "r1", vec![crate::fix_flags::FixFlag::CheckRemoved]);
        crate::risk_flags::settle(
            st,
            "r1",
            vec![crate::risk_flags::RiskFlag::NewScriptlet {
                header: "%post".into(),
            }],
        );
    })
    .unwrap();
}

#[test]
fn a_run_for_another_version_starts_without_the_last_versions_flags() {
    let state = tempfile::tempdir().unwrap();
    built_with_flags(state.path());
    let ws = Workspace::open(state.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| st.flags_version = Some("0.9".into())).unwrap();
    start_flags_for_version(&ws, "1.0", &ap_core::sink::NullReporter);
    let st = PkgState::load(&ws);
    assert!(st.fix_flags.is_empty() && st.risk_flags.is_empty());
    assert_eq!(st.flags_version.as_deref(), Some("1.0"));
}

#[test]
fn an_already_built_run_keeps_the_flags_of_the_revision_it_found() {
    let state = tempfile::tempdir().unwrap();
    built_with_flags(state.path());
    let mut config = config("foo", state.path());
    config.run_id = "r2".into();
    let tmp = tempfile::tempdir().unwrap();
    let run = ap_core::run::RunDir::allocate(tmp.path())
        .unwrap()
        .pkg(&config.rpm_name);
    let result = run_package(&config, &run, &ap_core::sink::NullReporter);
    assert!(
        matches!(result, PipelineResult::AlreadyBuilt { .. }),
        "{result:?}"
    );
    let ws = Workspace::open(state.path(), "python-foo").unwrap();
    let st = PkgState::load(&ws);
    assert_eq!(st.fix_flags, vec![crate::fix_flags::FixFlag::CheckRemoved]);
    assert_eq!(st.risk_flags.len(), 1, "{:?}", st.risk_flags);
}
