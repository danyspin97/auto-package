use crate::pipeline::check_paths::*;

#[test]
fn a_file_a_diff_creates_is_named_with_the_level_stripped() {
    let diff = "--- a/x.c\n+++ b/x.c\n@@ -1 +1 @@\n-a\n+b\n\
                --- /dev/null\n+++ b/test/parquet/new.test\n@@ -0,0 +1 @@\n+x\n";
    assert_eq!(created_paths(diff, 1), vec!["test/parquet/new.test"]);
    assert_eq!(
        created_paths(diff, 0),
        vec!["b/test/parquet/new.test"],
        "-p0 keeps the prefix"
    );
}

#[test]
fn a_deleted_or_modified_file_is_not_created() {
    let deleted = "--- a/gone.test\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
    assert!(created_paths(deleted, 1).is_empty());
    let modified = "--- a/x.test\t2026-01-01\n+++ b/x.test\t2026-01-02\n";
    assert!(created_paths(modified, 1).is_empty());
}

fn tree_without(_: &str) -> bool {
    false
}

#[test]
fn a_patch_file_in_the_checkout_makes_its_created_paths_shipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("fix.patch"),
        "--- /dev/null\n+++ b/test/parquet/a.test\n@@ -0,0 +1 @@\n+x\n",
    )
    .unwrap();
    let spec = "Name: foo\nVersion: 1\nPatch1: fix.patch\n%prep\n%autosetup -p1\n\
                %check\n./test/unittest \"test/parquet/a.test\"\n";
    assert_eq!(
        patch_created_paths(spec, dir.path()),
        vec!["test/parquet/a.test"]
    );
    let (introduced, inherited) = missing_check_path_split(spec, None, &|p: &str| {
        patch_created_paths(spec, dir.path()).iter().any(|c| c == p)
    });
    assert_eq!(
        introduced.len(),
        1,
        "test/unittest is not shipped, so it stays a problem"
    );
    assert!(inherited.is_empty());
    assert!(introduced[0].contains("test/unittest"));
    assert!(!introduced[0].contains("test/parquet/a.test"));
}

#[test]
fn a_missing_check_path_with_no_patch_still_fails() {
    let spec = "%check\n%pytest test/parquet/gone.test\n\n%files\n";
    let (introduced, inherited) = missing_check_path_split(spec, None, &tree_without);
    assert_eq!(introduced.len(), 1);
    assert!(inherited.is_empty());
}

#[test]
fn a_missing_path_the_origin_check_has_too_is_inherited() {
    let origin = "%check\n%pytest test/parquet/gone.test\n\n%files\n";
    let spec = "%check\n%pytest test/parquet/gone.test\n\n%files\n";
    let (introduced, inherited) = missing_check_path_split(spec, Some(origin), &tree_without);
    assert!(introduced.is_empty(), "{introduced:?}");
    assert_eq!(inherited.len(), 1);
}

#[test]
fn a_new_missing_path_next_to_an_inherited_one_is_introduced_alone() {
    let origin = "%check\n%pytest test/a/old.test\n\n%files\n";
    let spec = "%check\n%pytest test/a/old.test test/b/new.test\n\n%files\n";
    let (introduced, inherited) = missing_check_path_split(spec, Some(origin), &tree_without);
    assert_eq!(inherited.len(), 1);
    assert!(inherited[0].contains("test/a/old.test"));
    assert_eq!(introduced.len(), 1);
    assert!(introduced[0].contains("test/b/new.test"));
    assert!(!introduced[0].contains("test/a/old.test"));
}

#[test]
fn a_patch_renamed_by_its_fragment_is_read_under_that_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("fix-version.patch"),
        "--- /dev/null\n+++ b/test/parquet/a.test\n@@ -0,0 +1 @@\n+x\n",
    )
    .unwrap();
    let spec = "Name: foo\nVersion: 1\nPatch:  https://github.com/o/r/commit/8cf50c9177936e771e158ea9b249b4aa5e2cc1ee.patch#/fix-version.patch\n%prep\n%autosetup -p1\n";
    assert_eq!(
        patch_created_paths(spec, dir.path()),
        vec!["test/parquet/a.test"]
    );
}
