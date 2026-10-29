
use super::*;

/// A checkout at `<base>/pkg`. The base is its own temp dir so that any
/// moved-aside sibling stays inside it.
fn checkout_with(files_xml: &str, in_update: bool, local_file: bool) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("pkg");
    fs::create_dir_all(dir.join(".osc")).unwrap();
    fs::write(dir.join(".osc/_files"), files_xml).unwrap();
    if in_update {
        fs::create_dir(dir.join(".osc/_in_update")).unwrap();
    }
    if local_file {
        fs::write(dir.join("_service"), "").unwrap();
    }
    base
}

/// The same checkout directory always gets the same lock, and another
/// package's directory gets its own, so it is not held up.
#[test]
fn checkout_locks_are_per_directory() {
    let a = Path::new("/ap-test-checkout-lock/proj/a");
    let b = Path::new("/ap-test-checkout-lock/proj/b");
    assert!(Arc::ptr_eq(&checkout_dir_lock(a), &checkout_dir_lock(a)));
    let held = checkout_dir_lock(a);
    let _guard = held.lock().unwrap();
    assert!(!Arc::ptr_eq(&held, &checkout_dir_lock(b)));
    let other = checkout_dir_lock(b);
    assert!(other.try_lock().is_ok());
}

/// Calls one after another take the lock in turn: a finished call leaves
/// the directory free for the next one.
#[test]
fn a_directory_lock_is_free_after_its_holder_leaves() {
    let dir = Path::new("/ap-test-checkout-lock/proj/c");
    drop(checkout_dir_lock(dir).lock().unwrap());
    let lock = checkout_dir_lock(dir);
    assert!(lock.try_lock().is_ok());
}

fn sibling_names(base: &Path) -> Vec<String> {
    fs::read_dir(base)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("pkg.unfinished-"))
        .collect()
}

#[test]
fn an_empty_unfinished_checkout_is_removed() {
    let base = checkout_with("<directory />", true, false);
    let dir = base.path().join("pkg");
    discard_unfinished_checkout(&dir, Emit::quiet());
    assert!(!dir.exists());
    assert!(sibling_names(base.path()).is_empty());
}

#[test]
fn an_unfinished_checkout_with_untracked_files_is_moved_aside_not_deleted() {
    let base = checkout_with("<directory />", true, true);
    let dir = base.path().join("pkg");
    discard_unfinished_checkout(&dir, Emit::quiet());
    assert!(!dir.exists());
    let aside = sibling_names(base.path());
    assert_eq!(aside.len(), 1, "{aside:?}");
    let kept = base.path().join(&aside[0]);
    assert!(kept.join("_service").exists());
}

#[test]
fn a_completed_or_tracking_checkout_is_kept() {
    let tracking = checkout_with(
        r#"<directory name="p"><entry name="p.spec" md5="a" size="1"/></directory>"#,
        true,
        true,
    );
    let dir = tracking.path().join("pkg");
    discard_unfinished_checkout(&dir, Emit::quiet());
    assert!(dir.join("_service").exists());

    let finished = checkout_with("<directory />", false, true);
    let dir = finished.path().join("pkg");
    discard_unfinished_checkout(&dir, Emit::quiet());
    assert!(dir.join("_service").exists());
}

#[test]
fn the_moved_aside_copy_is_not_a_checkout_any_more() {
    let base = checkout_with("<directory />", true, true);
    let dir = base.path().join("pkg");
    discard_unfinished_checkout(&dir, Emit::quiet());
    let aside = base.path().join(&sibling_names(base.path())[0]);
    assert!(aside.join("_service").exists());
    assert!(!aside.join(".osc").exists());
}

#[test]
fn only_the_newest_moved_aside_copy_of_a_package_is_kept() {
    let base = checkout_with("<directory />", true, true);
    // Older copies of this package, and a different package whose name
    // only starts the same way; neither of those may be touched.
    for old in ["pkg.unfinished-1", "pkg.unfinished-2-1"] {
        fs::create_dir_all(base.path().join(old)).unwrap();
    }
    fs::create_dir_all(base.path().join("pkg-other.unfinished-1")).unwrap();
    let dir = base.path().join("pkg");
    discard_unfinished_checkout(&dir, Emit::quiet());

    let aside = sibling_names(base.path());
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert!(base.path().join(&aside[0]).join("_service").exists());
    assert!(base.path().join("pkg-other.unfinished-1").exists());
}

#[test]
fn moved_aside_copies_older_than_a_week_are_pruned_at_checkout_time() {
    let base = tempfile::tempdir().unwrap();
    let old = base.path().join("pkg.unfinished-1");
    let recent = base.path().join("pkg.unfinished-2");
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&recent).unwrap();
    let eight_days_ago = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
    fs::File::open(&old)
        .unwrap()
        .set_modified(eight_days_ago)
        .unwrap();

    prune_unfinished_asides(
        &base.path().join("pkg"),
        None,
        Some(UNFINISHED_ASIDE_MAX_AGE),
    );

    assert!(!old.exists());
    assert!(recent.exists());
}
