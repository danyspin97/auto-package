use super::patch_rewrite::*;
use std::collections::BTreeMap;

const BASE: &str = "Subject: fix\nUpstream: no\n---\n--- foo-1.0/x.c\t2020-01-01\n+++ foo-1.0/x.c\t2020-01-02\n@@ -1,3 +1,3 @@\n a\n-b\n+c\n d\n";

fn changed(old: &str, new: &str, level: u32) -> bool {
    code_changed(Some(old.as_bytes()), new.as_bytes(), level)
}

#[test]
fn equal_bytes_are_unchanged() {
    assert!(!changed(BASE, BASE, 1));
}

#[test]
fn a_header_change_is_not_a_rewrite() {
    let new = BASE.replace("Subject: fix", "Subject: a longer subject\nFrom: me");
    assert!(!changed(BASE, &new, 1));
}

#[test]
fn offsets_are_not_a_rewrite() {
    assert!(!changed(
        BASE,
        &BASE.replace("@@ -1,3 +1,3 @@", "@@ -40,3 +41,3 @@"),
        1
    ));
}

#[test]
fn context_lines_are_not_a_rewrite() {
    let new = BASE.replace(" a\n", " other\n").replace(" d\n", " e\n");
    assert!(!changed(BASE, &new, 1));
}

#[test]
fn index_lines_and_timestamps_are_not_a_rewrite() {
    let new = BASE
        .replace(
            "--- foo-1.0/x.c\t2020-01-01",
            "Index: foo-1.0/x.c\n====\n--- foo-1.0/x.c",
        )
        .replace("+++ foo-1.0/x.c\t2020-01-02", "+++ foo-1.0/x.c");
    assert!(!changed(BASE, &new, 1));
}

#[test]
fn a_path_prefix_counts_only_below_its_level() {
    let new = BASE.replace("foo-1.0/x.c", "b/x.c");
    assert!(!changed(BASE, &new, 1));
    assert!(changed(BASE, &new, 0));
}

#[test]
fn a_changed_line_a_hunk_or_a_deletion_is_a_rewrite() {
    assert!(changed(BASE, &BASE.replace("+c", "+z"), 1));
    let more = format!("{BASE}@@ -9 +9 @@\n-q\n+r\n");
    assert!(changed(BASE, &more, 1));
    let gone = format!("{BASE}--- foo-1.0/y.c\n+++ /dev/null\n@@ -1 +0,0 @@\n-y\n");
    assert!(changed(BASE, &gone, 1));
}

#[test]
fn a_moved_hunk_is_not_a_rewrite() {
    let a = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-p\n+q\n@@ -9 +9 @@\n-r\n+s\n";
    let b = "--- a/x\n+++ b/x\n@@ -9 +9 @@\n-r\n+s\n@@ -1 +1 @@\n-p\n+q\n";
    assert!(!changed(a, b, 1));
}

#[test]
fn a_new_patch_is_a_rewrite() {
    assert!(code_changed(None, BASE.as_bytes(), 1));
}

#[test]
fn rewritten_patches_lists_new_and_changed_patches_in_spec_order() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: foo\nVersion: 1\nPatch0: %{name}-b.patch\nPatch1: a.patch\n\
                Patch2: gone.patch\nPatch3: same.patch\nPatch4: done.patch\n%autosetup -p1\n";
    let cur = BASE.replace("+c", "+z");
    for (n, t) in [
        ("foo-b.patch", cur.as_str()),
        ("a.patch", cur.as_str()),
        ("same.patch", BASE),
        ("done.patch", cur.as_str()),
    ] {
        std::fs::write(dir.path().join(n), t).unwrap();
    }
    let mut before: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    before.insert("foo-b.patch".into(), Some(BASE.as_bytes().to_vec()));
    before.insert("same.patch".into(), Some(BASE.as_bytes().to_vec()));
    before.insert("done.patch".into(), Some(BASE.as_bytes().to_vec()));
    before.insert("gone.patch".into(), Some(BASE.as_bytes().to_vec()));
    let mut accepted = BTreeMap::new();
    accepted.insert("done.patch".to_string(), cur.as_bytes().to_vec());
    // gone.patch has no file now; it is declared but absent, so it is skipped.
    assert_eq!(
        rewritten_patches(spec, dir.path(), &before, &accepted),
        ["foo-b.patch", "a.patch"]
    );
    // A patch the spec no longer declares is not listed.
    let without = spec.replace("Patch1: a.patch\n", "");
    assert_eq!(
        rewritten_patches(&without, dir.path(), &before, &accepted),
        ["foo-b.patch"]
    );
}
