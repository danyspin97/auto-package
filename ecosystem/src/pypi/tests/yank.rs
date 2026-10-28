use super::*;

#[test]
fn a_yanked_file_taints_the_release() {
    let json = MARKUPSAFE.replacen(
        r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
      "yanked": false"#,
        r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
      "yanked": true"#,
        1,
    );
    assert!(parse_release(&json).unwrap().yanked);
    assert!(!markupsafe().yanked);
}

/// A release with one `.tar.gz` and one `.zip` sdist, each with its own
/// file-level `yanked` flag.
fn two_sdists(tar_yanked: bool, zip_yanked: bool) -> String {
    format!(
        r#"{{"info": {{"name": "pkg", "version": "1.0", "yanked": false}},
          "urls": [
            {{"digests": {{"sha256": "aaaa"}}, "filename": "pkg-1.0.tar.gz",
              "packagetype": "sdist", "size": 1,
              "url": "https://example.test/pkg-1.0.tar.gz", "yanked": {tar_yanked}}},
            {{"digests": {{"sha256": "bbbb"}}, "filename": "pkg-1.0.zip",
              "packagetype": "sdist", "size": 2,
              "url": "https://example.test/pkg-1.0.zip", "yanked": {zip_yanked}}}
          ]}}"#
    )
}

#[test]
fn a_yanked_tarball_yields_to_a_live_zip_sdist() {
    let r = parse_release(&two_sdists(true, false)).expect("parses");
    assert_eq!(r.sdist.filename, "pkg-1.0.zip");
    assert!(!r.yanked, "the chosen sdist is live, so the release is too");
}

#[test]
fn a_live_tarball_keeps_its_format_preference() {
    for zip_yanked in [false, true] {
        let r = parse_release(&two_sdists(false, zip_yanked)).expect("parses");
        assert_eq!(r.sdist.filename, "pkg-1.0.tar.gz");
        assert!(!r.yanked);
    }
}

#[test]
fn when_every_sdist_is_yanked_the_tarball_is_still_picked_and_taints_the_release() {
    let r = parse_release(&two_sdists(true, true)).expect("parses");
    assert_eq!(r.sdist.filename, "pkg-1.0.tar.gz");
    assert!(r.yanked);
}

#[test]
fn tarball_path_honours_the_file_level_yanked_flag() {
    let yanked = MARKUPSAFE.replacen(
        r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
      "yanked": false"#,
        r#""url": "https://files.pythonhosted.org/packages/7e/99/markupsafe-3.0.3.tar.gz",
      "yanked": true"#,
        1,
    );
    let r = parse_release_with_tarball(&yanked, "markupsafe-3.0.3.tar.gz").expect("parses");
    assert!(r.yanked, "a yanked tarball file must taint the release");
    assert_eq!(r.sdist.filename, "markupsafe-3.0.3.tar.gz");

    let live = parse_release_with_tarball(MARKUPSAFE, "markupsafe-3.0.3.tar.gz").expect("parses");
    assert!(!live.yanked);
}

#[test]
fn tarball_path_honours_the_release_level_yanked_flag() {
    let json = MARKUPSAFE.replacen(
        r#""yanked": false
  },"#,
        r#""yanked": true
  },"#,
        1,
    );
    let r = parse_release_with_tarball(&json, "markupsafe-3.0.3.tar.gz").expect("parses");
    assert!(r.yanked);
}

#[test]
fn unknown_fields_do_not_break_parsing() {
    let json = MARKUPSAFE.replace(
        r#""yanked": false
  },"#,
        r#""yanked": false, "some_future_field": {"a": 1}
  },"#,
    );
    assert!(parse_release(&json).is_ok());
}

#[test]
fn a_missing_digest_is_refused() {
    let json = MARKUPSAFE.replace(
        r#""sha256": "722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698""#,
        r#""blake2b": "unused""#,
    );
    assert!(matches!(
        parse_release(&json),
        Err(Error::NoDigest(f)) if f == "markupsafe-3.0.3.tar.gz"
    ));
}
