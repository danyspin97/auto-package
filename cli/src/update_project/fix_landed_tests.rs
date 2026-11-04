use super::fix_landed::landed;
use ap_build::backport::PreparedPatch;
use ap_build::bugfix::{FixEnd, FixJob, FixTask, FixedIn, Found};

const SPEC: &str =
    "Name: foo\nVersion: 1.0\nSource0: a.tar.gz\nPatch0: foo-fix.patch\n%prep\n%autosetup -p1\n";

fn task(patches: Vec<PreparedPatch>) -> FixTask {
    FixTask::Bug(FixJob {
        pkg: "foo".into(),
        refs: vec!["bsc#1234567".into()],
        description: None,
        bug_texts: Vec::new(),
        read: Vec::new(),
        not_read: Vec::new(),
        warnings: Vec::new(),
        upstream_repo: None,
        patches,
        candidates: Vec::new(),
        found: Found::Given,
        unmerged: Vec::new(),
        fixed_in: FixedIn::NoneYet,
        fetch_hosts: Vec::new(),
        nonce: "n".into(),
    })
}

fn staged() -> PreparedPatch {
    PreparedPatch {
        file_name: "foo-fix.patch".into(),
        bytes: b"x".to_vec(),
        origin: "o".into(),
        commit: None,
        reason: "r".into(),
    }
}

#[test]
fn a_staged_patch_the_branch_holds_and_applies_landed() {
    let t = task(vec![staged()]);
    assert!(
        landed(
            &t,
            Some(&end("fixed", Some("foo-fix.patch"))),
            SPEC,
            &|_| Some("x".into())
        )
        .is_ok()
    );
    assert!(
        landed(&t, Some(&end("fixed", None)), SPEC, &|_| Some(
            "changed".into()
        ))
        .is_ok()
    );
}

#[test]
fn a_missing_or_unapplied_patch_did_not_land() {
    let t = task(vec![staged()]);
    let e = landed(&t, Some(&end("fixed", None)), SPEC, &|_| None).unwrap_err();
    assert!(
        e.contains("the fix did not go in") && e.contains("no foo-fix.patch"),
        "{e}"
    );
    let off = "Name: foo\nVersion: 1.0\nPatch0: foo-fix.patch\n%prep\n%setup -q\n";
    let e = landed(&t, Some(&end("fixed", None)), off, &|_| Some("x".into())).unwrap_err();
    assert!(e.contains("does not apply"), "{e}");
}

#[test]
fn a_patch_the_turn_wrote_must_be_in_the_branch_and_applied() {
    let t = task(Vec::new());
    assert!(
        landed(
            &t,
            Some(&end("fixed", Some("foo-fix.patch"))),
            SPEC,
            &|_| Some("x".into())
        )
        .is_ok()
    );
    assert!(
        landed(
            &t,
            Some(&end("fixed", Some("foo-fix.patch"))),
            SPEC,
            &|_| None
        )
        .is_err()
    );
    assert!(
        landed(
            &t,
            Some(&end("fixed", Some("foo-fix.patch"))),
            "Name: foo\n",
            &|_| Some("x".into())
        )
        .is_err()
    );
    // A fixed record that names no patch and nothing staged: nothing more to check.
    assert!(landed(&t, Some(&end("fixed", None)), SPEC, &|_| None).is_ok());
}

fn end(verdict: &str, patch: Option<&str>) -> FixEnd {
    FixEnd {
        nonce: "n".into(),
        verdict: verdict.into(),
        patch: patch.map(str::to_string),
        applied: None,
        reason: String::new(),
    }
}

#[test]
fn nothing_lands_without_a_fixed_record_of_this_run() {
    let t = task(vec![staged()]);
    let e = landed(&t, None, SPEC, &|_| Some("x".into())).unwrap_err();
    assert!(e.contains("no fixed verdict"), "{e}");
    assert!(landed(&t, Some(&end("no_fix", None)), SPEC, &|_| Some("x".into())).is_err());
    let none = task(Vec::new());
    assert!(landed(&none, None, SPEC, &|_| None).is_err());
}
