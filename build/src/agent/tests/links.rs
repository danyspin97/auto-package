//! Tests for the symbolic links a turn leaves: a link that resolves into the
//! checkout's metadata, its root, the workspace outside `source/`, or outside the
//! trees, is undone and the turn rejected; a link inside the trees, or one there
//! before the turn, is left alone. Also that a turn which unwinds is still
//! settled, and that a chain of links is resolved in bounded time.

use crate::agent::integrity::TurnGuard;

/// A workspace with a `source/` directory, a checkout with a `.osc`, and a
/// file outside both.
struct Trees {
    base: tempfile::TempDir,
}

impl Trees {
    fn new() -> Trees {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("ws/source")).unwrap();
        std::fs::create_dir_all(base.path().join("co/.osc")).unwrap();
        std::fs::write(base.path().join("co/.osc/_apiurl"), "https://api.example\n").unwrap();
        std::fs::write(base.path().join("co/spec.spec"), "Name: x\n").unwrap();
        std::fs::write(base.path().join("ws/source/file.c"), "int x;\n").unwrap();
        std::fs::write(base.path().join("outside"), "o").unwrap();
        Trees { base }
    }

    fn ws(&self) -> std::path::PathBuf {
        self.base.path().join("ws")
    }

    fn co(&self) -> std::path::PathBuf {
        self.base.path().join("co")
    }

    /// Run `during` as a turn and return what the check rejected it for.
    fn turn(&self, during: impl FnOnce(&Trees)) -> Option<String> {
        let guard = TurnGuard::begin(&self.ws(), Some(&self.co()));
        during(self);
        guard.settle()
    }
}

fn link(target: &str, at: &std::path::Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

#[test]
fn a_link_from_source_into_the_checkouts_osc_is_undone() {
    let t = Trees::new();
    let detail = t
        .turn(|t| link("../../co/.osc", &t.ws().join("source/lnk")))
        .expect("a link into .osc is rejected");
    assert!(detail.contains("source/lnk"), "{detail}");
    assert!(std::fs::symlink_metadata(t.ws().join("source/lnk")).is_err());
}

#[test]
fn a_dangling_link_into_osc_is_undone_too() {
    let t = Trees::new();
    let detail = t.turn(|t| link("../../co/.osc/newfile", &t.ws().join("source/lnk")));
    assert!(detail.is_some());
    assert!(std::fs::symlink_metadata(t.ws().join("source/lnk")).is_err());
}

#[test]
fn a_link_chain_that_reaches_osc_through_another_link_is_undone() {
    let t = Trees::new();
    let detail = t.turn(|t| {
        link("../../co", &t.ws().join("source/a"));
        link("a/.osc/_apiurl", &t.ws().join("source/b"));
    });
    let detail = detail.expect("a link that reaches .osc through a link is rejected");
    assert!(detail.contains("source/b"), "{detail}");
    assert!(std::fs::symlink_metadata(t.ws().join("source/b")).is_err());
}

#[test]
fn a_link_to_a_file_outside_the_trees_is_undone() {
    let t = Trees::new();
    let outside = t.base.path().join("outside");
    let detail = t.turn(|t| link(outside.to_str().unwrap(), &t.ws().join("source/out")));
    assert!(detail.is_some());
    assert!(std::fs::symlink_metadata(t.ws().join("source/out")).is_err());
}

#[test]
fn a_link_retargeted_out_of_the_trees_gets_its_old_target_back() {
    let t = Trees::new();
    link("file.c", &t.ws().join("source/keep"));
    let detail = t.turn(|t| {
        std::fs::remove_file(t.ws().join("source/keep")).unwrap();
        link("../../co/.osc/_apiurl", &t.ws().join("source/keep"));
    });
    assert!(detail.is_some());
    assert_eq!(
        std::fs::read_link(t.ws().join("source/keep")).unwrap(),
        std::path::PathBuf::from("file.c")
    );
}

#[test]
fn links_inside_the_trees_and_the_pipelines_own_links_are_left_alone() {
    let t = Trees::new();
    // Before the turn: the pipeline's own link to the sdist, which points
    // outside the checkout, and an escaping one that the turn did not make.
    link("../../../../../etc", &t.ws().join("old"));
    let detail = t.turn(|t| {
        link("file.c", &t.ws().join("source/inside"));
        link("spec.spec", &t.co().join("alias.spec"));
    });
    assert_eq!(detail, None);
    assert!(std::fs::symlink_metadata(t.ws().join("source/inside")).is_ok());
    assert!(std::fs::symlink_metadata(t.co().join("alias.spec")).is_ok());
    assert!(std::fs::symlink_metadata(t.ws().join("old")).is_ok());
}

#[test]
fn a_link_under_the_checkouts_osc_is_a_metadata_change_not_a_link_one() {
    // The metadata check removes it; the links are not examined inside .osc.
    let t = Trees::new();
    let detail = t
        .turn(|t| link("/etc/passwd", &t.co().join(".osc/evil")))
        .expect("a link under .osc is a change");
    assert!(detail.contains("added .osc/evil"), "{detail}");
    assert!(std::fs::symlink_metadata(t.co().join(".osc/evil")).is_err());
}

#[test]
fn a_link_to_the_checkout_root_is_undone() {
    // Through it, `source/root/.osc/_apiurl` reaches the metadata.
    let t = Trees::new();
    let detail = t.turn(|t| link("../../co", &t.ws().join("source/root")));
    let detail = detail.expect("a link to the checkout root is rejected");
    assert!(detail.contains("source/root"), "{detail}");
    assert!(std::fs::symlink_metadata(t.ws().join("source/root")).is_err());
}

#[test]
fn a_link_into_the_workspace_outside_source_is_undone() {
    // The workspace's own files (state, notes, the OBS baseline) are the
    // pipeline's, not the turn's.
    let t = Trees::new();
    let detail = t.turn(|t| link("../notes.md", &t.ws().join("source/notes")));
    assert!(detail.is_some());
    assert!(std::fs::symlink_metadata(t.ws().join("source/notes")).is_err());
}

#[test]
fn a_link_to_a_checkout_series_or_changes_file_is_undone() {
    let t = Trees::new();
    std::fs::write(t.co().join("series"), "").unwrap();
    std::fs::write(t.co().join("pkg.changes"), "").unwrap();
    let detail = t.turn(|t| {
        link("../../co/series", &t.ws().join("source/s"));
        link("../../co/pkg.changes", &t.ws().join("source/c"));
    });
    assert!(detail.is_some());
    assert!(std::fs::symlink_metadata(t.ws().join("source/s")).is_err());
    assert!(std::fs::symlink_metadata(t.ws().join("source/c")).is_err());
}

#[test]
fn a_chain_of_links_fanning_out_is_refused_in_bounded_time() {
    // l_k -> l_(k-1)/l_(k-1) doubles the links a resolution visits at each step:
    // resolving l_30 unbounded takes about 2^30 steps.
    let t = Trees::new();
    let src = t.ws().join("source");
    link(".", &src.join("l0"));
    for k in 1..=30 {
        let prev = format!("l{}", k - 1);
        link(&format!("{prev}/{prev}"), &src.join(format!("l{k}")));
    }
    let started = std::time::Instant::now();
    let detail = t.turn(|t| link("l30", &t.ws().join("source/top")));
    assert!(detail.is_some(), "a link through the chain is refused");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(std::fs::symlink_metadata(t.ws().join("source/top")).is_err());
}

#[test]
fn a_turn_that_unwinds_is_still_undone() {
    let t = Trees::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = TurnGuard::begin(&t.ws(), Some(&t.co()));
        std::fs::write(t.co().join(".osc/_apiurl"), "http://evil\n").unwrap();
        link("../../co/.osc/_apiurl", &t.ws().join("source/lnk"));
        panic!("the turn died");
    }));
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(t.co().join(".osc/_apiurl")).unwrap(),
        b"https://api.example\n"
    );
    assert!(std::fs::symlink_metadata(t.ws().join("source/lnk")).is_err());
}
