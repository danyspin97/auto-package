use super::evidence_lines;

#[test]
fn at_most_three_short_items_on_one_line() {
    assert_eq!(evidence_lines(&[]), "");
    let long = "x".repeat(300);
    let out = evidence_lines(&[
        "build.log line 12:\n  error: boom".into(),
        long,
        "c".into(),
        "d".into(),
    ]);
    assert!(
        out.starts_with("Evidence: build.log line 12: error: boom | xxx"),
        "{out}"
    );
    assert!(out.ends_with("... | c\n"), "{out}");
    assert!(!out.contains(" | d"), "{out}");
}
