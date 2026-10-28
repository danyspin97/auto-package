use super::*;

const SAMPLE: &str = "\
changes files:
--------------
--- foo.changes
+++ foo.changes
@@ -1,2 +1,6 @@
+-------------------------------------------------------------------
+Mon Jan  1 00:00:00 UTC 2026 - Someone <a@b.c>
+
+- Update to 2.0
 -------------------------------------------------------------------
 Old entry
spec files:
-----------
--- foo.spec
+++ foo.spec
@@ -10,5 +10,5 @@ Name:           foo
 License:        MIT
 URL:            https://example.org
-Version:        1.0
+Version:        2.0
 Source0:        foo-%{version}.tar.gz
 BuildRequires:  gcc

Binary file 'foo-2.0.tar.gz' has changed.
";

#[test]
fn parses_files_hunks_and_counts() {
    let d = parse(SAMPLE);
    assert_eq!(d.files.len(), 3);
    assert_eq!(d.files[0].new, "foo.changes");
    assert_eq!((d.files[0].added, d.files[0].removed), (4, 0));
    assert_eq!((d.files[1].added, d.files[1].removed), (1, 1));
    assert_eq!(d.files[1].hunks[0].context, "Name:           foo");
    assert_eq!(d.added(), 5);
    assert_eq!(d.removed(), 1);
    assert_eq!(d.summary(), "3 files changed, +5 \u{2212}1, 1 binary");
}

#[test]
fn numbers_old_and_new_lines_independently() {
    let d = parse(SAMPLE);
    let h = &d.files[1].hunks[0];
    let by = |t: &str| h.lines.iter().find(|l| l.text.starts_with(t)).unwrap();
    assert_eq!(
        (by("License").old_no, by("License").new_no),
        (Some(10), Some(10))
    );
    let del = by("Version:        1");
    assert_eq!(
        (del.kind, del.old_no, del.new_no),
        (Kind::Del, Some(12), None)
    );
    let add = by("Version:        2");
    assert_eq!(
        (add.kind, add.old_no, add.new_no),
        (Kind::Add, None, Some(12))
    );
    assert_eq!(by("BuildRequires").old_no, Some(14));
}

#[test]
fn a_dashed_line_inside_a_hunk_is_content_not_a_header() {
    let d = parse("--- a.txt\n+++ a.txt\n@@ -1,2 +1,1 @@\n--- not a header\n keep\n");
    assert_eq!(d.files.len(), 1);
    assert_eq!(d.files[0].hunks[0].lines[0].text, "-- not a header");
    assert_eq!(d.files[0].removed, 1);
}

#[test]
fn binary_lines_and_archives_are_not_shown() {
    let d = parse(SAMPLE);
    assert_eq!(
        d.files[2].binary.as_deref(),
        Some("Binary file foo-2.0.tar.gz changed")
    );
    let d = parse("--- a.tar.gz\n+++ a.tar.gz\n@@ -1 +1 @@\n-aaaa\n+bbbb\n");
    let note = d.files[0].binary.as_deref().unwrap();
    assert!(note.starts_with("Binary file a.tar.gz changed ("), "{note}");
    assert!(d.files[0].hunks.is_empty());
    assert_eq!(d.added(), 0);
}

#[test]
fn a_very_large_diff_is_capped_with_a_notice() {
    let mut text = String::from("--- a\n+++ a\n@@ -0,0 +1,30000 @@\n");
    for i in 0..30_000 {
        text.push_str(&format!("+line {i}\n"));
    }
    let d = parse(&text);
    assert!(d.dropped_lines > 9_000);
    assert!(d.summary().ends_with("truncated"));
    let r = render(&d, 80);
    assert!(r.plain.last().unwrap().contains("diff truncated"));
}

#[test]
fn long_lines_are_capped_and_tabs_expand() {
    assert_eq!(clean("a\tb"), "a   b");
    assert_eq!(clean("\t"), "    ");
    let long = "x".repeat(5_000);
    assert!(clean(&long).chars().count() <= MAX_LINE_CHARS + 1);
    assert_eq!(clean("a\u{1}b"), "a\u{b7}b");
}

#[test]
fn a_diff_without_headers_is_one_anonymous_file() {
    let d = parse("@@ -1 +1 @@\n-a\n+b\n");
    assert_eq!(d.files[0].new, "(diff)");
    assert_eq!(d.files[0].hunks[0].lines.len(), 2);
}

#[test]
fn empty_input_has_no_changes() {
    let d = parse("");
    assert!(d.is_empty());
    assert_eq!(d.summary(), "no changes");
}

#[test]
fn new_and_deleted_files_are_titled() {
    let d = parse("--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+x\n");
    assert_eq!(d.files[0].title(), "n.txt (new file)");
}

#[test]
fn recorded_spec_diffs_parse_without_numbers() {
    let d = parse_recorded_all("foo.spec", &["- Version: 1\n+ Version: 2\n".to_string()]);
    assert_eq!((d.added(), d.removed()), (1, 1));
    assert_eq!(d.files[0].hunks[0].lines[0].old_no, None);
    assert!(
        render(&d, 60)
            .plain
            .iter()
            .any(|l| l.contains("Version: 2"))
    );
}

#[test]
fn word_diff_marks_only_the_changed_words() {
    let (o, n) = word_diff("Version:   1.0.3", "Version:   1.0.4").unwrap();
    assert_eq!(&"Version:   1.0.3"[o[0].clone()], "3");
    assert_eq!(&"Version:   1.0.4"[n[0].clone()], "4");
    assert!(word_diff("same", "same").is_none());
    assert!(word_diff("alpha", "beta").is_none());
}

#[test]
fn rendering_shows_gutter_signs_and_navigation_points() {
    let d = parse(SAMPLE);
    let r = render(&d, 80);
    assert_eq!(r.files.len(), 3);
    assert_eq!(r.hunks.len(), 2);
    assert!(r.plain[r.files[1]].contains("foo.spec"));
    let del = r
        .plain
        .iter()
        .find(|l| l.contains("Version:        1.0"))
        .unwrap();
    assert!(del.contains(" 12 ") && del.contains("- "), "{del}");
    let add = r
        .plain
        .iter()
        .find(|l| l.contains("Version:        2.0"))
        .unwrap();
    assert!(add.contains("+ "), "{add}");
    assert!(r.plain[r.hunks[1]].starts_with("@@ -10,5 +10,5 @@ Name:"));
    // A changed line carries emphasis on its changed part only.
    let line = r
        .lines
        .iter()
        .find(|l| l.to_string().contains("Version:        2.0"))
        .unwrap();
    assert!(
        line.spans
            .iter()
            .any(|s| s.style.bg == Some(ADD_EMPH) && s.content == "2")
    );
    assert!(line.spans.iter().any(|s| s.style.bg == Some(ADD_BG)));
}

#[test]
fn long_lines_wrap_to_the_width() {
    let text = format!("--- a\n+++ a\n@@ -1 +1 @@\n-x\n+{}\n", "word ".repeat(40));
    let r = render(&parse(&text), 40);
    assert!(r.lines.len() > 7);
    for l in &r.lines {
        assert!(l.width() <= 40, "{}", l.width());
    }
}

#[test]
fn jumps_and_search() {
    let s = [3, 9, 20];
    assert_eq!(next_start(&s, 3), Some(9));
    assert_eq!(next_start(&s, 20), None);
    assert_eq!(prev_start(&s, 9), Some(3));
    assert_eq!(prev_start(&s, 3), None);
    let plain: Vec<String> = ["alpha", "Beta", "gamma", "beta2"].map(String::from).into();
    assert_eq!(find_line(&plain, "BETA", 0, true), Some(1));
    assert_eq!(find_line(&plain, "beta", 1, true), Some(3));
    assert_eq!(find_line(&plain, "beta", 3, true), Some(1));
    assert_eq!(find_line(&plain, "beta", 1, false), Some(3));
    assert_eq!(find_line(&plain, "zzz", 0, true), None);
}
