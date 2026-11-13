use super::*;
use ap_build::pipeline::PkgState;
use ap_build::workspace::Workspace;

/// A work directory with one package whose state recorded a review of 2.8.4
/// and whose checkout is at 2.8.14 with a leftover archive of 2.8.4.
fn stale_work_dir(root: &Path) -> std::path::PathBuf {
    let work = root.join("work");
    let state_root = work.join("state");
    let checkout = work.join("proj").join("pyenv");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join("pyenv.spec"),
        "Name: pyenv\nVersion: 2.8.14\nSource0: pyenv-%{version}.tar.gz\n",
    )
    .unwrap();
    std::fs::write(checkout.join("pyenv-2.8.14.tar.gz"), "x").unwrap();
    std::fs::write(checkout.join("pyenv-2.8.4.tar.gz"), "x").unwrap();
    let ws = Workspace::open(&state_root, "pyenv").unwrap();
    PkgState::update(&ws, |s| {
        s.reviewed_version = Some("2.8.4".into());
        s.reviewed_spec_hash = Some(1);
    })
    .unwrap();
    work
}

#[test]
fn a_dry_run_changes_neither_the_state_nor_the_checkout() {
    let root = tempfile::tempdir().unwrap();
    let work = stale_work_dir(root.path());
    let checkout = work.join("proj").join("pyenv");

    let healed = heal_on_disk(&work, "proj", true, &|_: &str| true);

    assert!(healed.is_empty(), "{healed:?}");
    assert!(checkout.join("pyenv-2.8.4.tar.gz").exists());
    let ws = Workspace::find_existing(&work.join("state"), &["pyenv"]).unwrap();
    let st = PkgState::load(&ws);
    assert_eq!(st.reviewed_version.as_deref(), Some("2.8.4"));
    assert!(!ws.dir().join("reconciled.log").exists());
}

#[test]
fn a_run_heals_the_stale_review_and_the_leftover_archive() {
    let root = tempfile::tempdir().unwrap();
    let work = stale_work_dir(root.path());
    let checkout = work.join("proj").join("pyenv");

    let healed = heal_on_disk(&work, "proj", false, &|_: &str| true);

    assert!(!checkout.join("pyenv-2.8.4.tar.gz").exists());
    assert!(checkout.join("pyenv-2.8.14.tar.gz").exists());
    assert!(
        healed
            .iter()
            .any(|(n, w)| n == "pyenv" && w.contains("review of 2.8.4"))
    );
    let ws = Workspace::find_existing(&work.join("state"), &["pyenv"]).unwrap();
    assert_eq!(PkgState::load(&ws).reviewed_version, None);
}

#[test]
fn a_run_heals_only_the_packages_it_covers() {
    let root = tempfile::tempdir().unwrap();
    let work = stale_work_dir(root.path());
    let checkout = work.join("proj").join("pyenv");

    let healed = heal_on_disk(&work, "proj", false, &|n: &str| n == "other");

    assert!(healed.is_empty(), "{healed:?}");
    assert!(checkout.join("pyenv-2.8.4.tar.gz").exists());
    let ws = Workspace::find_existing(&work.join("state"), &["pyenv"]).unwrap();
    assert_eq!(
        PkgState::load(&ws).reviewed_version.as_deref(),
        Some("2.8.4")
    );
}
