use super::*;

fn end(nonce: &str) -> FixEnd {
    FixEnd {
        nonce: nonce.into(),
        verdict: "fixed".into(),
        patch: Some("foo-fix.patch".into()),
        applied: Some("clean".into()),
        reason: "applies".into(),
    }
}

#[test]
fn a_record_is_read_back_only_by_its_own_run() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(read_fix_end(d.path(), "a"), None);
    write_fix_end(d.path(), &end("a")).unwrap();
    assert_eq!(read_fix_end(d.path(), "a"), Some(end("a")));
    assert_eq!(read_fix_end(d.path(), "b"), None);
}

#[test]
fn a_record_that_does_not_parse_is_none_and_a_rewrite_replaces_it() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(FIX_END_FILE), "{ not json").unwrap();
    assert_eq!(read_fix_end(d.path(), "a"), None);
    write_fix_end(d.path(), &end("a")).unwrap();
    let mut second = end("a");
    second.verdict = "no_fix".into();
    write_fix_end(d.path(), &second).unwrap();
    assert_eq!(read_fix_end(d.path(), "a").unwrap().verdict, "no_fix");
}
