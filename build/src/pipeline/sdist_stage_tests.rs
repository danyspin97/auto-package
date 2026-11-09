use super::*;
use crate::pipeline::archive::find_sdist_top_level;
use crate::pipeline::sdist_stage::{fetch_sdist_stage, unpacked_top_level};

/// Discards every event.
struct Quiet;

impl Reporter for Quiet {
    fn report(&self, _: &Event) {}
}

/// A `.tar.gz` at `path` holding one small file for each of `entries`.
fn tarball(path: &Path, entries: &[&str]) {
    let file = std::fs::File::create(path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for name in entries {
        let body = b"x\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, &body[..]).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
}

#[test]
fn the_archive_directory_names_the_top_level_not_the_file() {
    let dir = tempfile::tempdir().unwrap();
    tarball(
        &dir.path().join("AMQPStorm-1.0.tar.gz"),
        &["amqpstorm-1.0/x"],
    );
    assert_eq!(
        find_sdist_top_level(dir.path(), "amqpstorm", "1.0"),
        "amqpstorm-1.0"
    );
}

#[test]
fn an_unreadable_tarball_falls_back_to_the_file_stem() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AMQPStorm-1.0.tar.gz"), b"not gzip").unwrap();
    assert_eq!(
        find_sdist_top_level(dir.path(), "amqpstorm", "1.0"),
        "AMQPStorm-1.0"
    );
}

#[test]
fn the_unpacked_directory_replaces_a_stale_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("amqpstorm-1.0")).unwrap();
    assert_eq!(
        unpacked_top_level(dir.path(), "AMQPStorm-1.0"),
        "amqpstorm-1.0"
    );
}

#[test]
fn a_name_that_exists_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg-1.0")).unwrap();
    std::fs::create_dir_all(dir.path().join("other")).unwrap();
    assert_eq!(unpacked_top_level(dir.path(), "pkg-1.0"), "pkg-1.0");
}

#[test]
fn several_entries_or_a_file_keep_the_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    assert_eq!(unpacked_top_level(dir.path(), "pkg-1.0"), "pkg-1.0");

    let file = tempfile::tempdir().unwrap();
    std::fs::write(file.path().join("pkg.py"), "").unwrap();
    assert_eq!(unpacked_top_level(file.path(), "pkg-1.0"), "pkg-1.0");
}

#[test]
fn an_empty_name_is_a_flat_unpack_and_stays_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("only")).unwrap();
    assert_eq!(unpacked_top_level(dir.path(), ""), "");
}

/// Runs the sdist stage on a checkout whose `AMQPStorm-1.0.tar.gz` holds
/// `entries`, starting from the top-level name `hint`. Returns the run's
/// temporary root, the workspace directory and the top-level name it ends with.
fn fetch(entries: &[&str], hint: &str) -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.work_dir = work.clone();
    config.version = "1.0".into();
    let checkout = work.join("p/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    tarball(&checkout.join("AMQPStorm-1.0.tar.gz"), entries);
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, "Name: foo\nVersion: 1.0\n").unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("foo");
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let ws_dir = ws.dir().to_path_buf();
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &Quiet,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path,
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: hint.into(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
        carried: None,
    };
    fetch_sdist_stage(&ctx, &mut st, false).unwrap_or_else(|_| panic!("the stage fails"));
    (root, ws_dir, st.sdist_top_level)
}

#[test]
fn the_source_link_follows_the_archive_directory() {
    let (_root, ws_dir, top) = fetch(&["amqpstorm-1.0/x"], "AMQPStorm-1.0");
    assert_eq!(top, "amqpstorm-1.0");
    let link = ws_dir.join("source");
    assert!(link.join("x").exists(), "source/ links the unpacked tree");
}

#[test]
fn a_tarball_with_several_top_entries_gets_no_link_to_a_guess() {
    let (_root, ws_dir, top) = fetch(&["amqpstorm-1.0/x", "docs/y"], "AMQPStorm-1.0");
    assert_eq!(top, "AMQPStorm-1.0");
    assert!(ws_dir.join("source").symlink_metadata().is_err());
}
