use super::hygiene_tests::config;
use super::*;

#[test]
fn a_package_whose_kept_baseline_does_not_restore_does_not_start() {
    let state = tempfile::tempdir().unwrap();
    let cfg = config("foo", state.path());
    let ws = Workspace::open(state.path(), &cfg.rpm_name).unwrap();
    let kept = ws.dir().join(OBS_BASELINE_DIR);
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(kept.join("files")).unwrap();
    std::fs::write(kept.join("checkout"), checkout.path().display().to_string()).unwrap();
    // An attempt whose file name is not plain: the restore refuses before
    // any OBS call, so nothing is written and the baseline must stay.
    osc::note_attempt(&kept, &["../escape".to_string()]).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let run = ap_core::run::RunDir::allocate(tmp.path())
        .unwrap()
        .pkg(&cfg.rpm_name);
    let result = run_package(&cfg, &run, &ap_core::sink::NullReporter);
    let PipelineResult::Failed { reason, .. } = result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert!(reason.contains(&kept.display().to_string()), "{reason}");
    assert!(kept.join("files").exists(), "the baseline was deleted");
}
