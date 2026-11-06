use super::osc_lock::*;
use super::*;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// A project working copy `root/<project>` with its `.osc/_packages` and one
/// package checkout `root/<project>/<pkg>` with its own `.osc`.
fn project_with_package(root: &Path, project: &str, pkg: &str) -> (PathBuf, PathBuf) {
    let proj = root.join(project);
    fs::create_dir_all(proj.join(".osc")).unwrap();
    fs::write(proj.join(".osc").join("_packages"), "<project/>").unwrap();
    let pkg_dir = proj.join(pkg);
    fs::create_dir_all(pkg_dir.join(".osc")).unwrap();
    fs::write(pkg_dir.join(".osc").join("_project"), project).unwrap();
    (proj, pkg_dir)
}

fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap()
}

#[test]
fn a_package_write_is_keyed_by_its_project_working_copy() {
    let root = tempfile::tempdir().unwrap();
    let (proj, pkg) = project_with_package(root.path(), "home:u", "python-foo");
    for sub in [
        "ci", "up", "add", "rm", "revert", "vc", "commit", "repairwc",
    ] {
        let key = lock_key("osc", &argv(&[sub, "-m", "x"]), &pkg);
        assert_eq!(key, Some(canon(&proj)), "osc {sub}");
    }
}

#[test]
fn two_packages_of_one_project_share_a_key_and_other_projects_do_not() {
    let root = tempfile::tempdir().unwrap();
    let (_, a) = project_with_package(root.path(), "home:u", "a");
    let (_, b) = project_with_package(root.path(), "home:u", "b");
    let (_, c) = project_with_package(root.path(), "home:v", "a");
    let ka = lock_key("osc", &argv(&["ci"]), &a);
    assert!(ka.is_some());
    assert_eq!(ka, lock_key("osc", &argv(&["up"]), &b));
    assert_ne!(ka, lock_key("osc", &argv(&["ci"]), &c));
}

#[test]
fn a_checkout_is_keyed_by_the_project_it_creates_in_its_directory() {
    let root = tempfile::tempdir().unwrap();
    let key = lock_key("osc", &argv(&["co", "home:u/python-foo"]), root.path());
    assert_eq!(key, Some(canon(root.path()).join("home:u")));
}

#[test]
fn reads_and_long_commands_take_no_lock() {
    let root = tempfile::tempdir().unwrap();
    let (_, pkg) = project_with_package(root.path(), "home:u", "a");
    for sub in [
        "ls",
        "cat",
        "api",
        "status",
        "results",
        "rdiff",
        "service",
        "build",
        "getbinaries",
    ] {
        assert_eq!(lock_key("osc", &argv(&[sub]), &pkg), None, "osc {sub}");
    }
}

#[test]
fn a_write_outside_a_project_working_copy_takes_no_lock() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(lock_key("osc", &argv(&["ci"]), root.path()), None);
}

#[test]
fn a_program_other_than_osc_takes_no_lock() {
    let root = tempfile::tempdir().unwrap();
    let (_, pkg) = project_with_package(root.path(), "home:u", "a");
    assert_eq!(lock_key("git", &argv(&["ci"]), &pkg), None);
    assert!(lock_key("/usr/bin/osc", &argv(&["up"]), &pkg).is_some());
}

#[test]
fn the_checkout_argument_must_name_one_project_and_package() {
    assert_eq!(
        checkout_project(&argv(&["-e", "-r", "rev", "home:u", "p", "f"])),
        None
    );
    assert_eq!(checkout_project(&argv(&["home:u/a/b"])), None);
    assert_eq!(checkout_project(&argv(&["/a"])), None);
    assert_eq!(checkout_project(&argv(&["home:u/a"])), Some("home:u"));
}

#[test]
fn packages_of_one_project_do_not_overlap() {
    let key = PathBuf::from("/work/home:u");
    let running = AtomicUsize::new(0);
    let most = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..5 {
                    with_lock(Some(&key), || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(2));
                        running.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        }
    });
    assert_eq!(most.load(Ordering::SeqCst), 1);
}

#[test]
fn a_held_project_does_not_block_another_project() {
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        s.spawn(move || {
            with_lock(Some(Path::new("/work/one")), || {
                held_tx.send(()).unwrap();
                let _ = release_rx.recv();
            });
        });
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        s.spawn(move || {
            with_lock(Some(Path::new("/work/two")), || ());
            done_tx.send(()).unwrap();
        });
        let other_done = done_rx.recv_timeout(Duration::from_secs(5));
        release_tx.send(()).unwrap();
        assert!(other_done.is_ok(), "another project waited for the lock");
    });
}

#[test]
fn a_panic_does_not_poison_the_lock() {
    let key = PathBuf::from("/work/poisoned");
    let r = std::panic::catch_unwind(|| with_lock(Some(&key), || panic!("boom")));
    assert!(r.is_err());
    assert_eq!(with_lock(Some(&key), || 7), 7);
}
