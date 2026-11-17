use super::*;

fn rewrite_of(tag: &str, version: &str) -> Option<String> {
    let xml = render_service_file("https://example.org/o/r", tag, version, "r", None);
    assert!(
        xml.contains(&format!("<param name=\"revision\">{tag}</param>")),
        "{xml}"
    );
    let pat = xml.split("versionrewrite-pattern\">").nth(1)?;
    assert!(xml.contains("versionrewrite-replacement\">\\1<"), "{xml}");
    Some(pat.split("</param>").next().unwrap().to_string())
}

#[test]
fn the_service_revision_is_the_tag_and_the_rewrite_cuts_its_prefix() {
    assert_eq!(rewrite_of("v1.2.3", "1.2.3").unwrap(), "^v(1\\.2\\.3)$");
    assert_eq!(
        rewrite_of("release-1.0", "1.0").unwrap(),
        "^release\\-(1\\.0)$"
    );
    assert_eq!(
        rewrite_of("release/1.0", "1.0").unwrap(),
        "^release/(1\\.0)$"
    );
    assert_eq!(rewrite_of("r-v1.0", "1.0").unwrap(), "^r\\-v(1\\.0)$");
    assert!(rewrite_of("5.0.4", "5.0.4").is_none());
    // The tag does not end with the version the spec gets (`1.0.0~rc1`):
    // no rewrite, so the tarball version stays `1.0.0-rc1`.
    assert!(rewrite_of("1.0.0-rc1", "1.0.0~rc1").is_none());
}

#[test]
fn the_rewrite_matches_only_the_tag_it_was_built_for() {
    let pat = rewrite_of("release-1.0", "1.0")
        .unwrap()
        .replace("\\-", "-");
    let re = regex::Regex::new(&pat).unwrap();
    assert_eq!(re.replace("release-1.0", "$1"), "1.0");
    assert!(!re.is_match("release-1x0"));
    assert!(!re.is_match("release-1.0.1"));
}

#[test]
fn the_service_tag_is_the_forges_or_an_announced_guess() {
    assert_eq!(
        service_tag(Some("5.0.4".into()), "5.0.4"),
        ("5.0.4".into(), None)
    );
    assert_eq!(
        service_tag(Some("release-1.0".into()), "1.0"),
        ("release-1.0".into(), None)
    );
    let (tag, msg) = service_tag(None, "1.0");
    assert_eq!(tag, "v1.0");
    assert!(msg.unwrap().contains("assuming the tag v1.0"));
}
