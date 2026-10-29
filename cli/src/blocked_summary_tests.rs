
use super::*;

#[test]
fn a_dependency_that_failed_reads_failed_in_the_waiting_list() {
    let torch = "python-torch".to_string();
    let deps = ["torch".to_string(), "numpy".to_string()];
    assert_eq!(waiting_list(&deps, &[&torch]), "failed torch, numpy");
    assert_eq!(waiting_list(&deps, &[]), "torch, numpy");
}

#[test]
fn the_summary_lists_the_rpmlint_errors_of_the_latest_build_only() {
    let root = tempfile::tempdir().unwrap();
    let ws = ap_build::workspace::Workspace::open(root.path(), "python-x").unwrap();
    std::fs::write(
        ws.dir().join("rpmlint.log"),
        "=== rpmlint of python-x 1 (unix time 1) ===\nx.rpm: E: old-error a\n\n\
             === rpmlint of python-x 2 (unix time 2) ===\nx.rpm: W: warn\nx.rpm: E: new-error b\n",
    )
    .unwrap();
    let pkg = "python-x".to_string();
    assert_eq!(
        rpmlint_errors_of(root.path(), &[&pkg]),
        ["python-x: x.rpm: E: new-error b"]
    );
}

#[test]
fn the_summary_lists_every_flavors_rpmlint_errors_of_the_latest_run() {
    let root = tempfile::tempdir().unwrap();
    let ws = ap_build::workspace::Workspace::open(root.path(), "python-x").unwrap();
    std::fs::write(
        ws.dir().join("rpmlint.log"),
        "=== rpmlint of python-x 1 (unix time 1) ===\nx.rpm: E: old-error a\n\n\
             === rpmlint of python-x 2 (unix time 2) ===\nx.rpm: E: base-error\n\n\
             === rpmlint of python-x:mpi 2 (unix time 2) ===\nm.rpm: E: mpi-error\n",
    )
    .unwrap();
    let pkg = "python-x".to_string();
    assert_eq!(
        rpmlint_errors_of(root.path(), &[&pkg]),
        [
            "python-x: x.rpm: E: base-error",
            "python-x: m.rpm: E: mpi-error"
        ]
    );
}

#[test]
fn a_spent_ladder_is_an_ordinary_failure() {
    let spent = PipelineResult::Failed {
        rpm_name: "python-torch".into(),
        reason: format!("{}x", ap_build::pipeline::LADDER_FAILED_PREFIX),
        kind: FailureKind::Exhausted,
    };
    assert!(matches!(
        package_outcome(&spent),
        PackageOutcome::Failed { .. }
    ));
    let failed = PipelineResult::Failed {
        rpm_name: "python-x".into(),
        reason: "no".into(),
        kind: FailureKind::Other,
    };
    assert!(matches!(
        package_outcome(&failed),
        PackageOutcome::Failed { .. }
    ));
}
