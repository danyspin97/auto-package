use super::*;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn a_bug_reference_gets_the_suse_spelling_and_is_kept_once() {
    let i = parse_bug_input(
        &s(&["boo#1234567,BSC#1234567", "bnc#7654321"]),
        &[],
        None,
        None,
    )
    .unwrap();
    assert_eq!(i.refs, ["boo#1234567", "bsc#1234567", "bnc#7654321"]);
}

#[test]
fn cve_takes_only_a_cve_and_bug_takes_one_too() {
    assert!(parse_bug_input(&[], &s(&["bsc#1234567"]), None, None).is_err());
    let i = parse_bug_input(&s(&["CVE-2025-1234"]), &s(&["cve-2025-1234"]), None, None).unwrap();
    assert_eq!(i.refs, ["CVE-2025-1234"]);
}

#[test]
fn one_of_the_four_inputs_is_required() {
    assert!(parse_bug_input(&[], &[], None, None).is_err());
    assert!(parse_bug_input(&[], &[], Some("  "), None).is_err());
    assert!(parse_bug_input(&[], &[], Some("crashes on empty input"), None).is_ok());
}

#[test]
fn a_long_description_is_refused() {
    let long = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
    let e = parse_bug_input(&[], &[], Some(&long), None).unwrap_err();
    assert!(e.contains("--bug-file"), "{e}");
}

#[test]
fn a_cve_without_a_bug_warns() {
    let i = parse_bug_input(&s(&["CVE-2025-1234"]), &[], None, None).unwrap();
    assert_eq!(i.warnings.len(), 1);
    let i = parse_bug_input(&s(&["CVE-2025-1234", "bsc#1234567"]), &[], None, None).unwrap();
    assert!(i.warnings.is_empty());
}

#[test]
fn a_bug_file_is_read_with_a_size_cap() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bug.txt");
    std::fs::write(&p, "heap overflow in parse()").unwrap();
    let i = parse_bug_input(&[], &[], None, Some(&p)).unwrap();
    assert_eq!(i.report_file.unwrap().text, "heap overflow in parse()");
    let big = dir.path().join("big.txt");
    std::fs::write(&big, vec![b'a'; MAX_BUG_FILE_BYTES as usize + 1]).unwrap();
    assert!(parse_bug_input(&[], &[], None, Some(&big)).is_err());
    assert!(parse_bug_input(&[], &[], None, Some(dir.path())).is_err());
    assert!(parse_bug_input(&[], &[], None, Some(&dir.path().join("none"))).is_err());
}
