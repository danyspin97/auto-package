//! The spec cache: a kept spec is served while its revision holds, a moved
//! revision is read again, and a failed or unstable read is never kept. A
//! corrupt file is a miss, no work directory means no cache, file names keep
//! projects and packages apart, and the sibling index over the cache answers as
//! the index over OBS does.

use super::siblings::{NO_REPORT, ProjectSiblings};
use super::spec_cache::{cached_read, file_part, path_of, spec_with};
use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The spec text of package `i`, on the same forge repository as package `i`
/// mod 3, so the index has siblings to find.
fn spec(i: usize) -> String {
    format!(
        "Name:           pkg-{i:02}\nVersion:        1.0\n\
         Source:         https://github.com/org/repo{}/archive/v1.0.tar.gz\n",
        i % 3
    )
}

fn rev(r: &str) -> Option<String> {
    Some(r.to_string())
}

fn no_obs() -> Option<String> {
    panic!("OBS must not be asked here")
}

#[test]
fn a_kept_spec_is_served_while_its_revision_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    let reads = Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok(spec(1))
    };
    assert_eq!(
        cached_read(&path, rev("r1"), &|| rev("r1"), &read).unwrap(),
        spec(1)
    );
    // The known revision answers the second read: OBS is not asked for it.
    assert_eq!(
        cached_read(&path, rev("r1"), &no_obs, &read).unwrap(),
        spec(1)
    );
    assert_eq!(reads.get(), 1);
}

#[test]
fn without_a_known_revision_the_one_asked_for_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    let reads = Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok(spec(1))
    };
    cached_read(&path, None, &|| rev("r1"), &read).unwrap();
    assert_eq!(
        cached_read(&path, None, &|| rev("r1"), &read).unwrap(),
        spec(1)
    );
    assert_eq!(reads.get(), 1);
}

#[test]
fn a_moved_revision_is_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    let reads = Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok(spec(reads.get()))
    };
    let first = cached_read(&path, rev("r1"), &|| rev("r1"), &read).unwrap();
    let second = cached_read(&path, rev("r2"), &|| rev("r2"), &read).unwrap();
    assert_eq!(reads.get(), 2);
    assert_ne!(first, second);
    assert_eq!(
        cached_read(&path, rev("r2"), &no_obs, &read).unwrap(),
        second
    );
    assert_eq!(reads.get(), 2);
}

#[test]
fn a_failed_read_is_never_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    let reads = Cell::new(0);
    let failing = || {
        reads.set(reads.get() + 1);
        Err(ReadError::Failed("timed out".into()))
    };
    assert!(cached_read(&path, rev("r1"), &|| rev("r1"), &failing).is_err());
    assert!(!path.exists());
    assert!(cached_read(&path, rev("r1"), &|| rev("r1"), &failing).is_err());
    assert_eq!(reads.get(), 2);
}

#[test]
fn a_revision_that_moves_during_the_read_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    let asks = AtomicUsize::new(0);
    // The revision is r1 before the read and r2 after it.
    let now = || {
        let n = asks.fetch_add(1, Ordering::Relaxed);
        rev(if n == 0 { "r1" } else { "r2" })
    };
    assert_eq!(
        cached_read(&path, None, &now, &|| Ok(spec(2))).unwrap(),
        spec(2)
    );
    assert!(!path.exists());
}

#[test]
fn no_revision_means_no_kept_spec() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    assert_eq!(
        cached_read(&path, None, &|| None, &|| Ok(spec(3))).unwrap(),
        spec(3)
    );
    assert!(!path.exists());
}

#[test]
fn a_corrupt_cache_file_is_a_miss_and_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pkg.json");
    for garbage in ["not json", "{\"revision\": 3}", "[]", ""] {
        std::fs::write(&path, garbage).unwrap();
        let reads = Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            Ok(spec(4))
        };
        assert_eq!(
            cached_read(&path, rev("r1"), &|| rev("r1"), &read).unwrap(),
            spec(4)
        );
        assert_eq!(reads.get(), 1, "a corrupt file {garbage:?} must be a miss");
        // The rewritten file is the good entry, served next time.
        assert_eq!(
            cached_read(&path, rev("r1"), &no_obs, &read).unwrap(),
            spec(4)
        );
        assert_eq!(reads.get(), 1);
    }
}

#[test]
fn no_work_directory_means_no_cache_and_no_revision_asked() {
    let reads = Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok(spec(5))
    };
    assert_eq!(
        spec_with(None, "p", "pkg", None, &no_obs, &read).unwrap(),
        spec(5)
    );
    assert_eq!(
        spec_with(None, "p", "pkg", rev("r1"), &no_obs, &read).unwrap(),
        spec(5)
    );
    assert_eq!(reads.get(), 2);
}

#[test]
fn the_work_directory_path_is_used_when_there_is_one() {
    let dir = tempfile::tempdir().unwrap();
    let read = || Ok(spec(6));
    spec_with(
        Some(dir.path()),
        "devel:languages:python",
        "python-Kivy",
        rev("r1"),
        &|| rev("r1"),
        &read,
    )
    .unwrap();
    assert!(path_of(dir.path(), "devel:languages:python", "python-Kivy").exists());
}

#[test]
fn file_names_keep_projects_and_packages_apart_and_stay_in_their_directory() {
    let names = [
        "a:b", "a_b", "a+b", "a%3Ab", "a%2Fb", "a/b", "a\\b", "a b", "..", ".", "...", "..x",
        "x/../y", "", "\u{e9}", "a\0b",
    ];
    let mut seen = std::collections::HashSet::new();
    for n in names {
        let part = file_part(n);
        assert!(
            !part.contains('/') && !part.contains('\\') && !part.contains('\0'),
            "{n:?} -> {part:?}"
        );
        assert!(part != "." && part != "..", "{n:?} -> {part:?}");
        assert!(seen.insert(part.clone()), "{n:?} collides on {part:?}");
    }
    let dir = std::path::Path::new("/work");
    for (project, pkg) in [("..", ".."), ("a/b", "../../etc"), ("p", "a/b")] {
        let path = path_of(dir, project, pkg);
        assert!(path.starts_with(dir.join("spec-cache")), "{path:?}");
        assert_eq!(
            path.components().count(),
            dir.components().count() + 3,
            "{path:?}"
        );
    }
    assert_ne!(path_of(dir, "a:b", "c"), path_of(dir, "a_b", "c"));
    assert_ne!(path_of(dir, "p", "a:b"), path_of(dir, "p", "a_b"));
}

/// The sibling index over OBS with `cache` (when given) in front of the reads:
/// each package's answer, and how many specs OBS was asked for.
fn index_answers(cache: Option<&std::path::Path>) -> (Vec<Vec<(String, String)>>, usize) {
    let names: Vec<String> = (0..12).map(|i| format!("pkg-{i:02}")).collect();
    let asked = AtomicUsize::new(0);
    let obs = |name: &str| {
        asked.fetch_add(1, Ordering::Relaxed);
        let i: usize = name["pkg-".len()..].parse().unwrap();
        Ok(spec(i))
    };
    let read = |name: &str| match cache {
        None => obs(name),
        Some(dir) => cached_read(
            &dir.join(format!("{name}.json")),
            rev(&format!("rev-{name}")),
            &|| rev(&format!("rev-{name}")),
            &|| obs(name),
        ),
    };
    let siblings = ProjectSiblings::new(names.clone(), 1, &read, &NO_REPORT);
    let answers = names
        .iter()
        .map(|n| siblings.related(n, "https://github.com/org/repo1").unwrap())
        .collect();
    (answers, asked.load(Ordering::Relaxed))
}

#[test]
fn the_index_over_the_cache_equals_the_uncached_index() {
    let dir = tempfile::tempdir().unwrap();
    let (uncached, _) = index_answers(None);
    let (cold, cold_reads) = index_answers(Some(dir.path()));
    let (warm, warm_reads) = index_answers(Some(dir.path()));
    assert_eq!(cold, uncached);
    assert_eq!(warm, uncached);
    assert_eq!(cold_reads, 12);
    assert_eq!(warm_reads, 0);
    assert!(uncached.iter().any(|s| !s.is_empty()));
}
