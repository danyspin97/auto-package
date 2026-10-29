use super::*;

#[test]
fn nothing_to_undo_is_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(undo_refs(&[]), None);
    assert_eq!(undo_refs(&[dir.path().join("never-written")]), None);
}

#[test]
fn a_baseline_that_cannot_go_back_is_named() {
    // An attempt was noted, but the checkout it was saved from is gone.
    let dir = tempfile::tempdir().unwrap();
    osc::note_attempt(dir.path(), &["foo.changes".to_string()]).unwrap();
    let why = undo_refs(&[dir.path().to_path_buf()]).unwrap();
    assert!(
        why.contains("checkout the baseline was saved from"),
        "{why}"
    );
}
