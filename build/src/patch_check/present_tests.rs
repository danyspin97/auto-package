//! The hunk proof of [`already_in_tarball`] for removals, deletions and fuzz:
//! the cases quilt cannot judge at fuzz 0, made of small synthetic archives.

use super::tests::tarball;
use super::*;

/// A one-file archive for `f.txt`, with `body` as its content.
fn one_file(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let tar = tarball(dir.path(), &[("f.txt", body)]);
    (dir, tar)
}

#[test]
fn a_hunk_that_only_removes_is_proven_by_its_context_and_the_gone_line() {
    // The line between two context lines is gone from the archive.
    let (_dir, tar) = one_file("int keep_me = 1;\nint other_line = 2;\n");
    let removal = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,2 @@\n int keep_me = 1;\n-int gone_line = 9;\n int other_line = 2;\n";
    assert!(already_in_tarball(&tar, removal.as_bytes(), 1));
    // The removed line is still there: not proven.
    let (_dir2, still) = one_file("int keep_me = 1;\nint gone_line = 9;\nint other_line = 2;\n");
    assert!(!already_in_tarball(&still, removal.as_bytes(), 1));
    // The context is not there (the lines around the removal differ): not proven.
    let (_dir3, other) = one_file("int keep_me = 1;\nint changed_line = 3;\n");
    assert!(!already_in_tarball(&other, removal.as_bytes(), 1));
    // A removal of only trivial lines proves nothing.
    let trivial =
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,2 @@\n int keep_me = 1;\n-}\n int other_line = 2;\n";
    assert!(!already_in_tarball(&tar, trivial.as_bytes(), 1));
}

#[test]
fn a_hunk_that_deletes_a_file_is_proven_when_the_file_is_gone() {
    let deletion = "--- a/old.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-int dropped_file = 1;\n-int another = 2;\n";
    // The archive does not have old.txt: the deletion is already upstream.
    let (_dir, tar) = one_file("int unrelated_line = 5;\n");
    assert!(already_in_tarball(&tar, deletion.as_bytes(), 1));
    // The archive still has it: not proven.
    let dir = tempfile::tempdir().unwrap();
    let kept = tarball(dir.path(), &[("old.txt", "int dropped_file = 1;\n")]);
    assert!(!already_in_tarball(&kept, deletion.as_bytes(), 1));
}

#[test]
fn every_hunk_of_a_patch_must_be_proven() {
    // A removal and a deletion, both proven: the patch is.
    let (_dir, tar) = one_file("int keep_me = 1;\nint other_line = 2;\n");
    let both = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,2 @@\n int keep_me = 1;\n-int gone_line = 9;\n int other_line = 2;\n\
                --- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-int dropped_file = 1;\n";
    assert!(already_in_tarball(&tar, both.as_bytes(), 1));
    // One hunk is not in the archive (its file has a line that is not gone): the
    // whole patch is not proven.
    let dir = tempfile::tempdir().unwrap();
    let tar2 = tarball(
        dir.path(),
        &[
            ("f.txt", "int keep_me = 1;\nint other_line = 2;\n"),
            ("gone.txt", "int dropped_file = 1;\n"),
        ],
    );
    assert!(!already_in_tarball(&tar2, both.as_bytes(), 1));
}

#[test]
fn a_fuzzy_hunk_is_proven_when_its_added_and_removed_lines_are_in_the_archive() {
    // The context differs from the archive (as a fuzz match does) but the
    // changed lines are the same: the change is in the archive.
    let (_dir, tar) = one_file("import os\nDeprecationWarning('x')\nint answer = 42;\nend\n");
    let fuzzy = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n import os\n-int answer = 41;\n+int answer = 42;\n context_that_is_not_there\n";
    assert!(already_in_tarball(&tar, fuzzy.as_bytes(), 1));
    // The added line differs from the archive: not proven.
    let differs = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n import os\n-int answer = 41;\n+int answer = 43;\n context_that_is_not_there\n";
    assert!(!already_in_tarball(&tar, differs.as_bytes(), 1));
}

#[test]
fn a_created_file_keeps_the_rule_of_its_added_lines() {
    let dir = tempfile::tempdir().unwrap();
    let created = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+int created_line = 1;\n";
    // The file is not in the archive: no proof.
    let (_dir2, absent) = one_file("int unrelated_line = 5;\n");
    assert!(!already_in_tarball(&absent, created.as_bytes(), 1));
    // The archive has the file with the line: the change is there.
    let tar = tarball(dir.path(), &[("new.txt", "int created_line = 1;\n")]);
    assert!(already_in_tarball(&tar, created.as_bytes(), 1));
}
