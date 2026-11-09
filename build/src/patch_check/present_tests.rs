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
    // One hunk is not in the archive (its deleted file is still there): the
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

#[test]
fn a_hunk_with_two_change_groups_is_proven_by_its_whole_post_image() {
    // The context between the two groups is part of the run: one, added, kept,
    // two and end, in that order.
    let (_dir, tar) = one_file(
        "int one_value = 1;\nint added_first = 9;\nint kept_line = 2;\nint two_value = 2;\nint end_line = 3;\n",
    );
    let groups = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,5 @@\n int one_value = 1;\n+int added_first = 9;\n int kept_line = 2;\n-int old_second = 5;\n+int two_value = 2;\n int end_line = 3;\n";
    assert!(already_in_tarball(&tar, groups.as_bytes(), 1));
    // The kept line between the groups is missing from the archive: the second
    // group is not next to the first, so not proven.
    let (_dir2, apart) = one_file(
        "int one_value = 1;\nint added_first = 9;\nint two_value = 2;\nint end_line = 3;\n",
    );
    assert!(!already_in_tarball(&apart, groups.as_bytes(), 1));
}

#[test]
fn a_removed_line_that_is_elsewhere_in_the_file_does_not_block_the_proof() {
    // `gone_line` is still in the file, but not in the post-image's run.
    let (_dir, tar) = one_file(
        "int gone_line = 9;\nint keep_me = 1;\nint answer_new = 42;\nint tail_line = 3;\n",
    );
    let moved = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n int keep_me = 1;\n-int gone_line = 9;\n+int answer_new = 42;\n";
    assert!(already_in_tarball(&tar, moved.as_bytes(), 1));
}

#[test]
fn trailing_context_that_differs_is_trimmed_but_three_lines_are_too_many() {
    let (_dir, tar) = one_file("int first_line = 1;\nint answer_new = 42;\nint tail_line = 3;\n");
    // One trailing context line differs from the archive: it is trimmed.
    let one = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n int first_line = 1;\n-int answer_old = 41;\n+int answer_new = 42;\n int tail_context = 9;\n";
    assert!(already_in_tarball(&tar, one.as_bytes(), 1));
    // Three trailing context lines differ: trimming two leaves one that does not
    // match, so not proven.
    let three = "--- a/f.txt\n+++ b/f.txt\n@@ -1,5 +1,5 @@\n int first_line = 1;\n-int answer_old = 41;\n+int answer_new = 42;\n int t_one = 1;\n int t_two = 2;\n int t_three = 3;\n";
    assert!(!already_in_tarball(&tar, three.as_bytes(), 1));
}

#[test]
fn a_removal_whose_removed_line_still_follows_its_context_is_not_proven() {
    // The removed line sits right after its context: the archive still has the
    // old lines, so the removal is not in it.
    let (_dir, tar) = one_file("int keep_me = 1;\nint gone_line = 9;\n");
    let removal =
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1 @@\n int keep_me = 1;\n-int gone_line = 9;\n";
    assert!(!already_in_tarball(&tar, removal.as_bytes(), 1));
}

#[test]
fn a_comment_with_a_typo_fixed_upstream_is_proven_but_a_code_change_is_not() {
    // The added comment has a typo the archive corrected (the pyenv case): it
    // matches the file's comment, the added code line matches exactly.
    let (_dir, tar) = one_file(
        "before_line = 1;\n# Look in each of the implementations for a version file.\nfind_local_version_file \"${PYENV_DIR:=$PWD}\" || {\nafter_line = 2;\n",
    );
    let typo = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,4 @@\n before_line = 1;\n+# Look in each of the implemntations for a version file.\n+find_local_version_file \"${PYENV_DIR:=$PWD}\" || {\n after_line = 2;\n";
    assert!(already_in_tarball(&tar, typo.as_bytes(), 1));
    // One code character differs: a code line matches only exactly.
    let code = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,4 @@\n before_line = 1;\n+# Look in each of the implemntations for a version file.\n+find_local_version_file \"${PYENV_DIR:=$CWD}\" || {\n after_line = 2;\n";
    assert!(!already_in_tarball(&tar, code.as_bytes(), 1));
    // A comment that differs in more than a typo is not the same comment.
    let other = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,4 @@\n before_line = 1;\n+# Search the parent directories for a version file here.\n+find_local_version_file \"${PYENV_DIR:=$PWD}\" || {\n after_line = 2;\n";
    assert!(!already_in_tarball(&tar, other.as_bytes(), 1));
    // A hunk of comment lines alone proves nothing: it needs an added code line.
    let only_comment = "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,3 @@\n before_line = 1;\n+# Look in each of the implemntations for a version file.\n after_line = 2;\n";
    assert!(!already_in_tarball(&tar, only_comment.as_bytes(), 1));
}
