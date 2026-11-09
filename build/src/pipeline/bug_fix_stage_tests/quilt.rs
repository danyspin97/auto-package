//! The quilt checks of the bug fix stage: the fix's patch is applied to the
//! real sources with quilt, when quilt is installed.

use super::*;

#[test]
fn quilt_applies_a_real_upstream_mail_on_a_small_tarball() {
    if quilt_missing() {
        return;
    }
    let case = Rig::new().run(good_job(), &mut run_patch_check, &mut *accept());
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.events[1].applied.as_deref(), Some("clean"));
}

#[test]
fn quilt_reverse_applies_a_patch_whose_change_is_in_the_sources() {
    if quilt_missing() {
        return;
    }
    // `B` is already in f.txt: the reverse of this patch is what the sources have.
    let job = job(vec![prepared(
        NAME,
        patch_bytes("--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n"),
    )]);
    let case = Rig::new().run(job, &mut run_patch_check, &mut |_, _| panic!("no review"));
    let (kind, _) = failed(&case);
    assert_eq!(*kind, FailureKind::NoFix);
}

#[test]
fn another_patch_of_the_same_name_is_not_taken_for_this_fix() {
    let mut rig = Rig::new();
    rig.spec.push_str(&format!("Patch0: {NAME}\n"));
    // Declared and present, with other code (or none): a maintainer's, not ours.
    for body in ["--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n", "not a patch"] {
        rig.existing = Some(body.into());
        let case = rig.run(good_job(), &mut |_, _| panic!("no check"), &mut |_, _| {
            panic!("no review")
        });
        let (kind, reason) = failed(&case);
        assert_eq!(*kind, FailureKind::NeedsMaintainer, "{reason}");
        assert!(reason.contains("not this fix's"), "{reason}");
    }
    // Declared but not in the checkout at all.
    rig.existing = None;
    let case = rig.run(good_job(), &mut |_, _| panic!("no check"), &mut |_, _| {
        panic!("no review")
    });
    assert_eq!(*failed(&case).0, FailureKind::NeedsMaintainer);
}
