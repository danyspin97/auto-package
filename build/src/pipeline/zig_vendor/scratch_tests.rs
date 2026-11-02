use super::scratch::Scratch;

#[test]
fn a_scratch_directory_is_removed_with_its_contents_when_dropped() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("zig-vendor");
    let scratch = Scratch::create(path.clone()).unwrap();
    std::fs::create_dir(path.join("dep-0")).unwrap();
    std::fs::write(path.join("dep-0/file"), b"x").unwrap();
    assert!(path.join("dep-0/file").exists());
    drop(scratch);
    assert!(!path.exists());
}

#[test]
fn an_existing_path_is_never_taken_over() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("keep"), b"k").unwrap();
    assert!(Scratch::create(work.path().join("keep")).is_err());
    assert_eq!(std::fs::read(work.path().join("keep")).unwrap(), b"k");
}

#[test]
fn a_failed_unpack_leaves_no_stage_or_root_behind() {
    use super::unpack::{Format, LIMITS, unpack};
    let work = tempfile::tempdir().unwrap();
    let dest = work.path().join("dep-0");
    // Not a tar: the reader fails after the stage is made.
    let err = unpack(&b"not an archive"[..], Format::Tar, &dest, LIMITS);
    assert!(err.is_err());
    assert!(!dest.exists());
    assert!(!work.path().join("dep-0.stage").exists());
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
}
