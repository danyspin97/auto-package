
use super::*;

#[test]
fn obs_names_reject_options_paths_and_junk() {
    for ok in [
        "python-Events",
        "libsigc++",
        "home:dspinella:python3",
        "a_b.c-1",
    ] {
        assert!(is_valid_obs_name(ok), "{ok}");
    }
    for bad in ["", "-rf", "../x", "a/b", "a b", "a\nb", "x..y", "/abs"] {
        assert!(!is_valid_obs_name(bad), "{bad:?}");
    }
}

#[test]
fn a_capability_that_looks_like_an_option_is_never_queried() {
    // Returns before touching zypper or the cache.
    assert!(!distro_provides("--repo"));
    assert!(!distro_provides(" -x"));
}

#[test]
fn parse_project_repo_paths_unescapes_names() {
    let xml = concat!(
        r#"<path project="Tools&amp;Co" repository="openSUSE_&quot;Tumbleweed&quot;"/>"#,
        r#"<path project="A&lt;B" repository="standard"/>"#,
        r#"<path project="Tools&amp;Co" repository="openSUSE_&quot;Tumbleweed&quot;"/>"#,
    );
    assert_eq!(
        parse_project_repo_paths(xml),
        vec![
            (
                "Tools&Co".to_string(),
                "openSUSE_\"Tumbleweed\"".to_string()
            ),
            ("A<B".to_string(), "standard".to_string()),
        ]
    );
}
