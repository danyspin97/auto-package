use super::*;

#[test]
fn replaces_the_one_match_and_keeps_every_other_byte() {
    // Latin-1 `é` (0xE9) and CRLF line endings: neither is valid UTF-8.
    let content = b"caf\xe9 one\r\nkeep \xff\xfe\r\nfoo bar\r\n";
    let out = replace_once_in(content, b"foo bar", b"baz").unwrap();
    assert_eq!(out, b"caf\xe9 one\r\nkeep \xff\xfe\r\nbaz\r\n");
}

#[test]
fn ascii_fragment_matches_inside_a_legacy_file() {
    let content = b"\xe9t\xe9 = 1\n";
    let out = replace_once_in(content, b"= 1", b"= 2").unwrap();
    assert_eq!(out, b"\xe9t\xe9 = 2\n");
}

#[test]
fn an_empty_new_text_deletes_the_match() {
    let out = replace_once_in(b"a\nDROP\nb\n", b"DROP\n", b"").unwrap();
    assert_eq!(out, b"a\nb\n");
}

#[test]
fn zero_matches_is_an_error_with_the_count() {
    let err = replace_once_in(b"nothing here\n", b"absent", b"x").unwrap_err();
    assert!(matches!(err, Error::Matches { count: 0, .. }), "{err:?}");
    assert!(err.to_string().contains("0 matches"), "{err}");
}

#[test]
fn several_matches_is_an_error_listing_the_offsets() {
    let err = replace_once_in(b"ab ab ab\n", b"ab", b"x").unwrap_err();
    assert!(matches!(err, Error::Matches { count: 3, .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("3 matches"), "{text}");
    assert!(text.contains("byte offsets 0, 3, 6"), "{text}");
}

#[test]
fn overlapping_occurrences_count_as_several() {
    // `aa` at 0 and at 1: replacing either one is a guess.
    let err = replace_once_in(b"aaa", b"aa", b"b").unwrap_err();
    assert!(matches!(err, Error::Matches { count: 2, .. }), "{err:?}");
}

#[test]
fn an_empty_search_text_is_refused() {
    let err = replace_once_in(b"abc", b"", b"x").unwrap_err();
    assert!(matches!(err, Error::EmptyOld), "{err:?}");
}

#[test]
fn file_edit_writes_bytes_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mysqldump.result");
    std::fs::write(&path, b"\xe9 old\r\n\xff\r\n").unwrap();
    replace_once(&path, b"old", b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"\xe9 new\r\n\xff\r\n");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("mysqldump.result")]);
}

#[test]
fn a_refused_file_edit_leaves_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    std::fs::write(&path, b"x x").unwrap();
    let err = replace_once(&path, b"x", b"y").unwrap_err();
    assert!(err.to_string().contains("2 matches"), "{err}");
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "{err}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"x x");
}

#[cfg(unix)]
#[test]
fn file_edit_keeps_the_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    std::fs::write(&path, b"echo old\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    replace_once(&path, b"old", b"new").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    assert_eq!(std::fs::read(&path).unwrap(), b"echo new\n");
}

#[test]
fn a_missing_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = replace_once(&dir.path().join("absent"), b"a", b"b").unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
}
