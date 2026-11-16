//! The scope of the startup reconcile: a run with `--only` reads and changes
//! only the packages it covers.

use super::*;

fn write_state(root: &Path, name: &str, outcome: &str, reason: Option<&str>) {
    let ws = Workspace::open(root, name).unwrap();
    PkgState::update(&ws, |s| {
        s.last_outcome = Some(outcome.into());
        s.last_failure_reason = reason.map(String::from);
    })
    .unwrap();
}

fn outcome(root: &Path, name: &str) -> Option<String> {
    PkgState::load(&Workspace::open(root, name).unwrap()).last_outcome
}

#[test]
fn reconcile_state_changes_only_the_packages_it_covers() {
    let root = tempfile::tempdir().unwrap();
    let in_scope = ["python-x", "python-y", "python-w"];
    // In scope: x waits on y, which is built, so x is freed.
    write_state(
        root.path(),
        "python-x",
        WAITING_ON_FAILED,
        Some("waiting on failed package: python-y"),
    );
    write_state(root.path(), "python-y", "success", None);
    // In scope, but its blocker is out of scope: not read, so it keeps waiting.
    write_state(
        root.path(),
        "python-w",
        WAITING_ON_FAILED,
        Some("waiting on failed package: python-torch"),
    );
    // Out of scope: a waiter on a built package and a missing_deps that would
    // be freed. Neither is changed by a run that does not cover it.
    write_state(
        root.path(),
        "python-o",
        WAITING_ON_FAILED,
        Some("waiting on failed package: python-y"),
    );
    write_state(
        root.path(),
        "python-m",
        "missing_deps",
        Some("waiting on dependencies: python-y"),
    );
    write_state(root.path(), "python-torch", "failed", Some("needs a GPU"));

    let covers = |n: &str| in_scope.contains(&n);
    let changed = reconcile_state(root.path(), covers);
    assert_eq!(changed, vec![("python-x".to_string(), "interrupted")]);
    assert_eq!(
        outcome(root.path(), "python-x").as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        outcome(root.path(), "python-w").as_deref(),
        Some(WAITING_ON_FAILED)
    );
    assert_eq!(
        outcome(root.path(), "python-o").as_deref(),
        Some(WAITING_ON_FAILED)
    );
    assert_eq!(
        outcome(root.path(), "python-m").as_deref(),
        Some("missing_deps")
    );
    assert_eq!(
        outcome(root.path(), "python-torch").as_deref(),
        Some("failed")
    );
    assert_eq!(outcome(root.path(), "python-y").as_deref(), Some("success"));
}

#[test]
fn reconcile_with_obs_fetches_only_the_packages_it_covers() {
    let root = tempfile::tempdir().unwrap();
    let state_root = root.path().join("state");
    let checkouts = root.path().join("home:u:p");
    for name in ["python-a", "python-b"] {
        write_state(&state_root, name, "failed", Some("OBS: build failed"));
        let ws = Workspace::open(&state_root, name).unwrap();
        PkgState::update(&ws, |s| s.last_built_version = Some("1.0".into())).unwrap();
        std::fs::create_dir_all(checkouts.join(name)).unwrap();
        std::fs::write(
            checkouts.join(name).join(format!("{name}.spec")),
            format!("Name: {name}\nVersion: 1.0\n"),
        )
        .unwrap();
    }
    let statuses: std::collections::HashMap<String, osc::BuildStatus> = [
        ("python-a".to_string(), osc::BuildStatus::Succeeded),
        ("python-b".to_string(), osc::BuildStatus::Succeeded),
    ]
    .into();
    let downloads = std::sync::Mutex::new(Vec::new());
    let rpms_dir = root.path().join("rpms");
    let done = reconcile_with_obs(
        &state_root,
        &checkouts,
        &rpms_dir,
        &statuses,
        |_| false,
        |name| {
            downloads.lock().unwrap().push(name.to_string());
            Ok(1)
        },
        |n| n == "python-a",
    );
    assert_eq!(
        downloads.into_inner().unwrap(),
        vec!["python-a".to_string()]
    );
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].0, "python-a");
    assert_eq!(outcome(&state_root, "python-a").as_deref(), Some("success"));
    assert_eq!(outcome(&state_root, "python-b").as_deref(), Some("failed"));
}
