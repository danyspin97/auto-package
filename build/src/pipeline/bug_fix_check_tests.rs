use crate::pipeline::bug_fix_check::check_fix;

const REFS: [&str; 2] = ["bsc#1234567", "CVE-2025-1234"];
const NAME: &str = "foo-CVE-2025-1234.patch";
const CODE_DIFF: &str = "--- a/src/parse.c\n+++ b/src/parse.c\n@@ -1,2 +1,3 @@\n a\n+b\n c\n";
const TEST_DIFF: &str = "--- a/tests/parse.c\n+++ b/tests/parse.c\n@@ -1,2 +1,3 @@\n a\n+b\n c\n";

fn refs() -> Vec<String> {
    REFS.iter().map(|r| r.to_string()).collect()
}

fn header(upstream: &str) -> String {
    format!(
        "Subject: check the length\nReferences: bsc#1234567 CVE-2025-1234\nUpstream: {upstream}\n\nDescription: backport\n---\n"
    )
}

fn spec(comment: &str, extra: &str) -> String {
    format!(
        "Name: foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n{comment}\nPatch0: {NAME}\n{extra}\n%prep\n%autosetup -p1\n"
    )
}

const COMMENT: &str =
    "# PATCH-FIX-UPSTREAM foo-CVE-2025-1234.patch bsc#1234567 CVE-2025-1234 -- check";

fn checkout(patch: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(NAME), patch).unwrap();
    d
}

fn good() -> String {
    format!(
        "{}{CODE_DIFF}",
        header("merged https://github.com/o/r/commit/abc1234")
    )
}

#[test]
fn a_good_fix_is_its_patch() {
    let d = checkout(&good());
    assert_eq!(
        check_fix(&spec(COMMENT, ""), d.path(), &refs(), Some(NAME), &[]),
        Ok(NAME.to_string())
    );
    // Found as the one patch the stage added.
    assert_eq!(
        check_fix(&spec(COMMENT, ""), d.path(), &refs(), None, &[]),
        Ok(NAME.to_string())
    );
}

#[test]
fn the_comment_must_exist_and_name_every_reference() {
    let d = checkout(&good());
    let e = check_fix(&spec("", ""), d.path(), &refs(), Some(NAME), &[]).unwrap_err();
    assert!(e.contains("PATCH-FIX-UPSTREAM"), "{e}");
    let partial = "# PATCH-FIX-UPSTREAM foo-CVE-2025-1234.patch CVE-2025-1234 -- check";
    let e = check_fix(&spec(partial, ""), d.path(), &refs(), Some(NAME), &[]).unwrap_err();
    assert!(e.contains("bsc#1234567"), "{e}");
    // A comment of another line is not the one above the patch.
    let apart = format!("{COMMENT}\n# unrelated");
    assert!(check_fix(&spec(&apart, ""), d.path(), &refs(), Some(NAME), &[]).is_err());
}

#[test]
fn a_header_problem_is_reported() {
    let d = checkout(&format!("{CODE_DIFF}"));
    let e = check_fix(&spec(COMMENT, ""), d.path(), &refs(), Some(NAME), &[]).unwrap_err();
    assert!(e.starts_with(NAME) && e.contains("Subject"), "{e}");
}

#[test]
fn a_patch_that_changes_only_tests_is_no_fix() {
    let d = checkout(&format!(
        "{}{TEST_DIFF}",
        header("merged https://github.com/o/r/commit/abc1234")
    ));
    let e = check_fix(&spec(COMMENT, ""), d.path(), &refs(), Some(NAME), &[]).unwrap_err();
    assert!(e.contains("only tests"), "{e}");
}

#[test]
fn two_new_patches_or_none_are_refused_without_a_staged_name() {
    let d = checkout(&good());
    std::fs::write(d.path().join("other.patch"), good()).unwrap();
    let two = spec(COMMENT, "Patch1: other.patch");
    let e = check_fix(&two, d.path(), &refs(), None, &[]).unwrap_err();
    assert!(e.contains("one fix, one patch"), "{e}");
    let e = check_fix(
        &spec(COMMENT, ""),
        d.path(),
        &refs(),
        None,
        &[NAME.to_string()],
    )
    .unwrap_err();
    assert!(e.contains("adds no PatchN"), "{e}");
    // The old patch is not the new one.
    let one_new = check_fix(&two, d.path(), &refs(), None, &["other.patch".to_string()]);
    assert_eq!(one_new, Ok(NAME.to_string()));
}

#[test]
fn a_patch_the_spec_does_not_apply_is_no_fix() {
    let d = checkout(&good());
    let s =
        "Name: foo\nVersion: 1.0\nSource0: a\nPatch0: foo-CVE-2025-1234.patch\n%prep\n%setup -q\n";
    let e = check_fix(s, d.path(), &refs(), Some(NAME), &[]).unwrap_err();
    assert!(e.contains("does not apply it"), "{e}");
}
