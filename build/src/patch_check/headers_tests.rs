//! The header prefix rewrite of [`rewrite_header_prefixes`], on synthetic patches.

use super::*;

const TOP: &str = "genshi-0.7.8";

fn rewrite(text: &str) -> Option<String> {
    rewrite_header_prefixes(text.as_bytes(), TOP).map(|b| String::from_utf8(b).unwrap())
}

#[test]
fn source_prefix_becomes_the_top_directory() {
    let before = "Index: source/genshi/core.py\n\
        ===================================================================\n\
        --- source/genshi/core.py\n\
        +++ source/genshi/core.py\n\
        @@ -10,2 +10,2 @@ class Stream(object):\n\
         keep\n\
        -old\n\
        +new\n";
    let after = rewrite(before).expect("rewritten");
    assert_eq!(
        after,
        before
            .replace("Index: source/", "Index: genshi-0.7.8/")
            .replace("--- source/", "--- genshi-0.7.8/")
            .replace("+++ source/", "+++ genshi-0.7.8/")
    );
    assert!(after.contains("@@ -10,2 +10,2 @@ class Stream(object):"));
}

#[test]
fn source_orig_prefix_becomes_the_top_orig() {
    let before = "--- source.orig/a.py\n+++ source/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(
        rewrite(before).unwrap(),
        "--- genshi-0.7.8.orig/a.py\n+++ genshi-0.7.8/a.py\n@@ -1 +1 @@\n-x\n+y\n"
    );
}

#[test]
fn a_timestamp_after_the_path_is_kept() {
    let before = "--- source/a.py\t2026-01-01 00:00:00.000000000 +0000\n+++ source/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(
        rewrite(before).unwrap(),
        "--- genshi-0.7.8/a.py\t2026-01-01 00:00:00.000000000 +0000\n+++ genshi-0.7.8/a.py\n@@ -1 +1 @@\n-x\n+y\n"
    );
}

#[test]
fn a_patch_with_a_and_b_paths_is_left_alone() {
    let before = "--- a/source/a.py\n+++ b/source/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(rewrite(before), None);
    let index_ab = "Index: a/x\n--- source/x\n+++ source/x\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(rewrite(index_ab), None);
}

#[test]
fn other_prefixes_are_untouched() {
    let other_top = "--- genshi-0.7.8/a.py\n+++ genshi-0.7.8/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(rewrite(other_top), None);
    let similar = "--- sources/a.py\n+++ sources/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(rewrite(similar), None);
    let devnull = "--- /dev/null\n+++ source/new.py\n@@ -0,0 +1 @@\n+x\n";
    assert_eq!(
        rewrite(devnull).unwrap(),
        "--- /dev/null\n+++ genshi-0.7.8/new.py\n@@ -0,0 +1 @@\n+x\n"
    );
}

#[test]
fn a_hunk_line_that_looks_like_a_header_is_not_rewritten() {
    // The removed line `-- source/x` reads as `--- source/x`; the `+++` after it
    // is a hunk line too (`++ y`), so neither is a header.
    let before =
        "--- source/a.sql\n+++ source/a.sql\n@@ -1,2 +1,2 @@\n--- source/x\n-y\n+++ source/z\n+w\n";
    assert_eq!(
        rewrite(before).unwrap(),
        "--- genshi-0.7.8/a.sql\n+++ genshi-0.7.8/a.sql\n@@ -1,2 +1,2 @@\n--- source/x\n-y\n+++ source/z\n+w\n"
    );
    let body_only = "--- a.sql\n+++ a.sql\n@@ -1,2 +1,2 @@\n--- source/x\n+++ source/z\n-y\n+w\n";
    assert_eq!(rewrite(body_only), None);
}

#[test]
fn the_text_above_the_diff_is_kept() {
    let before = "Subject: fix the parser\n\nsource/ is mentioned in prose.\n\n--- source/a.py\n+++ source/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    let after = rewrite(before).unwrap();
    assert!(after.starts_with("Subject: fix the parser\n\nsource/ is mentioned in prose.\n\n"));
    assert!(after.ends_with("--- genshi-0.7.8/a.py\n+++ genshi-0.7.8/a.py\n@@ -1 +1 @@\n-x\n+y\n"));
}

#[test]
fn the_rewrite_is_idempotent() {
    let before = "Index: source/a.py\n--- source/a.py\n+++ source/a.py\n@@ -1 +1 @@\n-x\n\\ No newline at end of file\n+y\n";
    let once = rewrite(before).unwrap();
    assert_eq!(rewrite(&once), None);
}

#[test]
fn an_unusable_top_or_a_non_utf8_patch_gives_nothing() {
    let text = "--- source/a.py\n+++ source/a.py\n@@ -1 +1 @@\n-x\n+y\n";
    for top in ["", "source", "source.orig", "a", "b", "x/y", "bad name\t"] {
        assert_eq!(
            rewrite_header_prefixes(text.as_bytes(), top),
            None,
            "top {top:?}"
        );
    }
    assert_eq!(rewrite_header_prefixes(&[0xff, 0xfe, b'\n'], TOP), None);
}

#[test]
fn a_top_that_only_starts_with_source_is_usable() {
    let text = "--- source/a.c\n+++ source/a.c\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(
        rewrite_header_prefixes(text.as_bytes(), "source-highlight-3.1")
            .map(|b| String::from_utf8(b).unwrap()),
        Some(
            "--- source-highlight-3.1/a.c\n+++ source-highlight-3.1/a.c\n@@ -1 +1 @@\n-x\n+y\n"
                .to_string()
        )
    );
}

#[test]
fn the_literal_source_and_source_orig_tops_are_refused() {
    let text = "--- source/a.c\n+++ source/a.c\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(rewrite_header_prefixes(text.as_bytes(), "source"), None);
    assert_eq!(
        rewrite_header_prefixes(text.as_bytes(), "source.orig"),
        None
    );
}

#[test]
fn crlf_line_endings_are_kept() {
    let before = "Index: source/a.py\r\n--- source/a.py\r\n+++ source/a.py\r\n@@ -1,2 +1,2 @@ def f():\r\n keep\r\n-old\r\n+new\r\n";
    assert_eq!(
        rewrite(before).unwrap(),
        before
            .replace("Index: source/", "Index: genshi-0.7.8/")
            .replace("--- source/", "--- genshi-0.7.8/")
            .replace("+++ source/", "+++ genshi-0.7.8/")
    );
}

#[test]
fn a_last_line_without_a_newline_is_kept() {
    let before = "--- source/a.py\n+++ source/a.py\n@@ -1 +1 @@\n-x\n+y";
    assert_eq!(
        rewrite(before).unwrap(),
        "--- genshi-0.7.8/a.py\n+++ genshi-0.7.8/a.py\n@@ -1 +1 @@\n-x\n+y"
    );
}

#[test]
fn every_file_of_a_multi_file_patch_is_rewritten() {
    let before = "Index: source/a.py\n--- source/a.py\n+++ source/a.py\n@@ -1 +1 @@\n-x\n+y\n\
        Index: source/pkg/b.py\n--- source/pkg/b.py\n+++ source/pkg/b.py\n@@ -1,2 +1 @@\n-p\n-q\n+r\n\
        --- source.orig/c.py\n+++ source/c.py\n@@ -3 +3 @@\n-s\n+t\n";
    let after = rewrite(before).unwrap();
    assert_eq!(
        after,
        "Index: genshi-0.7.8/a.py\n--- genshi-0.7.8/a.py\n+++ genshi-0.7.8/a.py\n@@ -1 +1 @@\n-x\n+y\n\
        Index: genshi-0.7.8/pkg/b.py\n--- genshi-0.7.8/pkg/b.py\n+++ genshi-0.7.8/pkg/b.py\n@@ -1,2 +1 @@\n-p\n-q\n+r\n\
        --- genshi-0.7.8.orig/c.py\n+++ genshi-0.7.8/c.py\n@@ -3 +3 @@\n-s\n+t\n"
    );
    assert_eq!(rewrite(&after), None);
}
