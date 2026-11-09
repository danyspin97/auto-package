use super::*;

const HEAD: &str = "8cc13f95bfbd9469b56a99644611c3ac";
const OWN: &str = "5c67e9c82c4b4410c0331cd3900c7394";

fn latest(user: &str, comment: &str) -> LatestRevision {
    LatestRevision {
        rev: "2".into(),
        srcmd5: OWN.into(),
        user: user.into(),
        time: 1_794_239_724,
        comment: comment.into(),
    }
}

/// python-Cramjam: the tool's commit to a package with a `_service` was never
/// recorded; OBS holds it as the expanded head, the checkout holds the same.
#[test]
fn an_own_attempt_commit_the_baseline_never_recorded_is_adopted() {
    let l = latest("dspinella", "auto-package: Update to 2.14.0 * Opt-in");
    assert_eq!(
        own_attempt_commit(HEAD, HEAD, Some(OWN), &l, "dspinella", &[]),
        Ok(())
    );
}

#[test]
fn a_commit_by_another_user_is_refused() {
    let l = latest("someone", "auto-package: Update to 2.14.0");
    let why = own_attempt_commit(HEAD, HEAD, Some(OWN), &l, "dspinella", &[]).unwrap_err();
    assert!(why.contains("someone"), "{why}");
}

#[test]
fn a_commit_with_another_message_is_refused() {
    let l = latest("dspinella", "fix the spec by hand");
    let why = own_attempt_commit(HEAD, HEAD, Some(OWN), &l, "dspinella", &[]).unwrap_err();
    assert!(why.contains("fix the spec by hand"), "{why}");
}

#[test]
fn a_head_the_checkout_does_not_hold_is_refused() {
    let l = latest("dspinella", "auto-package: Update to 2.14.0");
    assert!(
        own_attempt_commit(
            HEAD,
            "015dbda0d164436c0ea7c10f5d4ae032",
            Some(OWN),
            &l,
            "dspinella",
            &[]
        )
        .is_err()
    );
}

#[test]
fn a_revision_already_recorded_is_not_adopted_again() {
    let l = latest("dspinella", "auto-package: Update to 2.14.0");
    assert!(
        own_attempt_commit(HEAD, HEAD, Some(OWN), &l, "dspinella", &[HEAD.to_string()]).is_err()
    );
}

/// The link's base moved: the newest revision is not the package's own.
#[test]
fn a_newest_revision_that_is_not_the_package_own_is_refused() {
    let l = latest("dspinella", "auto-package: Update to 2.14.0");
    assert!(
        own_attempt_commit(
            HEAD,
            HEAD,
            Some("1bab7af08d87e68fc8a0bb2161fbab87"),
            &l,
            "dspinella",
            &[]
        )
        .is_err()
    );
    assert!(own_attempt_commit(HEAD, HEAD, None, &l, "dspinella", &[]).is_err());
}

#[test]
fn own_srcmd5_of_a_link_is_its_own_revision_not_the_expanded_one() {
    let xml = format!(
        "<directory name=\"python-cramjam\" rev=\"{HEAD}\" srcmd5=\"{HEAD}\">\n  \
         <linkinfo project=\"devel:languages:python\" package=\"python-cramjam\" \
         srcmd5=\"1bab\" xsrcmd5=\"{HEAD}\" lsrcmd5=\"{OWN}\"/>\n</directory>"
    );
    assert_eq!(own_srcmd5(&xml).as_deref(), Some(OWN));
    let plain = format!("<directory name=\"x\" srcmd5=\"{HEAD}\"></directory>");
    assert_eq!(own_srcmd5(&plain).as_deref(), Some(HEAD));
}
