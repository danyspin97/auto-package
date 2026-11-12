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

/// A package with the spec flag and a blocked flag of run `r1`.
fn blocked_by_r1(root: &Path) -> Workspace {
    built_with_flags(root);
    let ws = Workspace::open(root, "python-foo").unwrap();
    PkgState::update(&ws, |st| {
        st.flags_version = Some("1.0".into());
        crate::fix_flags::add_blocked(st, "r1", "no toolchain".into());
    })
    .unwrap();
    ws
}

fn blocked_count(ws: &Workspace) -> usize {
    PkgState::load(ws)
        .fix_flags
        .iter()
        .filter(|f| matches!(f, crate::fix_flags::FixFlag::Blocked { .. }))
        .count()
}

#[test]
fn a_blocked_flag_survives_a_run_start() {
    let state = tempfile::tempdir().unwrap();
    let ws = blocked_by_r1(state.path());
    start_flags_for_version(&ws, "1.0", &ap_core::sink::NullReporter);
    assert_eq!(blocked_count(&ws), 1);
}

#[test]
fn an_agent_turn_of_the_same_run_keeps_the_blocked_flag() {
    let state = tempfile::tempdir().unwrap();
    let ws = blocked_by_r1(state.path());
    let mut config = config("foo", state.path());
    config.run_id = "r1".into();
    clear_blocked_flags(&config, "python-foo", &ap_core::sink::NullReporter);
    assert_eq!(blocked_count(&ws), 1);
}

#[test]
fn an_agent_turn_of_a_later_run_clears_it_and_keeps_other_kinds() {
    let state = tempfile::tempdir().unwrap();
    let ws = blocked_by_r1(state.path());
    let mut config = config("foo", state.path());
    config.run_id = "r2".into();
    clear_blocked_flags(&config, "python-foo", &ap_core::sink::NullReporter);
    let st = PkgState::load(&ws);
    assert_eq!(st.fix_flags, vec![crate::fix_flags::FixFlag::CheckRemoved]);
}

#[test]
fn clearing_without_a_blocked_flag_writes_no_state() {
    let state = tempfile::tempdir().unwrap();
    built_with_flags(state.path());
    let ws = Workspace::open(state.path(), "python-foo").unwrap();
    let file = ws.state_json();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    let lock = ws.dir().join("state.json.lock");
    let had_lock = lock.exists();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut config = config("foo", state.path());
    config.run_id = "r2".into();
    clear_blocked_flags(&config, "python-foo", &ap_core::sink::NullReporter);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(lock.exists(), had_lock);
}
