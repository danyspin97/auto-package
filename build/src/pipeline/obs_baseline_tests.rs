use super::*;
use ap_core::sink::NullReporter;

/// A run directory whose baseline records one attempted commit.
fn with_attempt() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(OBS_BASELINE_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    osc::note_attempt(&dir, &["foo.spec".to_string()]).unwrap();
    (tmp, dir)
}

const DISCARD: &str = "auto-package baseline discard foo --project home:p --work-dir /ws";

fn failed(reason: &str) -> PipelineResult {
    PipelineResult::Failed {
        rpm_name: "foo".into(),
        reason: reason.into(),
        kind: FailureKind::Other,
    }
}

#[test]
fn a_failed_run_that_committed_is_restored_and_its_baseline_cleared() {
    let (_tmp, dir) = with_attempt();
    let mut restored = false;
    let out = restore_obs_baseline(&dir, &NullReporter, failed("build failed"), DISCARD, |_| {
        restored = true;
        Ok(crate::osc::Restored::Undone)
    });
    assert!(restored);
    match out {
        PipelineResult::Failed { reason, .. } => assert_eq!(reason, "build failed"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.exists());
}

#[test]
fn a_run_that_never_committed_restores_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(OBS_BASELINE_DIR);
    let out = restore_obs_baseline(&dir, &NullReporter, failed("no spec"), DISCARD, |_| {
        unreachable!("nothing was written, so nothing is restored")
    });
    assert!(matches!(out, PipelineResult::Failed { .. }));
}

#[test]
fn a_failed_restore_is_said_in_the_reason_and_the_baseline_is_kept() {
    let (_tmp, dir) = with_attempt();
    let out = restore_obs_baseline(&dir, &NullReporter, failed("build failed"), DISCARD, |_| {
        Err("no saved copy of the baseline".into())
    });
    match out {
        PipelineResult::Failed { reason, .. } => {
            assert!(reason.starts_with("build failed; "), "{reason}");
            assert!(reason.contains("NOT restored"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert!(dir.exists());
}

#[test]
fn the_baseline_is_read_from_the_files_save_baseline_keeps() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(OBS_BASELINE_DIR);
    std::fs::create_dir_all(dir.join("files")).unwrap();
    std::fs::write(dir.join("files").join("foo.spec"), "Name: foo\n").unwrap();
    assert_eq!(
        baseline_text(&dir, "foo.spec").as_deref(),
        Some("Name: foo\n")
    );
    assert_eq!(baseline_text(&dir, "bar.spec"), None);
}

#[test]
fn a_blocked_run_keeps_its_result_when_the_restore_fails() {
    let (_tmp, dir) = with_attempt();
    let blocked = PipelineResult::MissingDeps {
        rpm_name: "foo".into(),
        deps: vec!["bar".into()],
        patches: vec![],
        native: vec![],
        outdated: vec![],
    };
    let out = restore_obs_baseline(&dir, &NullReporter, blocked, DISCARD, |_| Err("x".into()));
    assert!(matches!(out, PipelineResult::MissingDeps { .. }), "{out:?}");
}

#[test]
fn a_refused_restore_names_the_command_that_discards_the_baseline() {
    let (_tmp, dir) = with_attempt();
    let out = restore_obs_baseline(&dir, &NullReporter, failed("build failed"), DISCARD, |_| {
        Err("OBS moved on".into())
    });
    match out {
        PipelineResult::Failed { reason, .. } => {
            assert!(
                reason.ends_with(&format!("discard the baseline with `{DISCARD}`")),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
}
