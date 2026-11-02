//! What a dependency's `url` names: a forge archive, a git commit to fetch with
//! git, or a refusal.

use super::fetch_source::{Source, source_of};
use super::fetch_tests::HEX40;

/// The archive a URL is fetched as; panics if it is not an archive.
fn archive_of(url: &str) -> String {
    match source_of(url).unwrap() {
        Source::Archive { url, .. } => url,
        other => panic!("{url} is not an archive: {other:?}"),
    }
}

#[test]
fn forge_git_urls_map_to_their_commit_archive() {
    let c = HEX40;
    let cases = [
        (
            format!("git+https://codeberg.org/ifreund/zig-wlroots#{c}"),
            format!("https://codeberg.org/ifreund/zig-wlroots/archive/{c}.tar.gz"),
        ),
        (
            format!("git+https://codeberg.org/ifreund/translate-c?ref=0.16.x#{c}"),
            format!("https://codeberg.org/ifreund/translate-c/archive/{c}.tar.gz"),
        ),
        (
            format!("git+https://github.com/o/r#{c}"),
            format!("https://codeload.github.com/o/r/tar.gz/{c}"),
        ),
        (
            format!("git+https://gitlab.com/o/r#{c}"),
            format!("https://gitlab.com/o/r/-/archive/{c}/r-{c}.tar.gz"),
        ),
        (
            format!("git+https://git.sr.ht/~o/r#{c}"),
            format!("https://git.sr.ht/~o/r/archive/{c}.tar.gz"),
        ),
    ];
    for (git, want) in cases {
        assert_eq!(archive_of(&git), want, "{git}");
    }
}

#[test]
fn git_urls_that_are_not_pinned_or_not_well_formed_are_refused() {
    let c = HEX40;
    let refused = [
        "git+https://codeberg.org/o/r".to_string(),
        "git+https://codeberg.org/o/r#v0.3.0".to_string(),
        format!("git+https://codeberg.org/o/r#{}", &c[..12]),
        format!("git+https://codeberg.org/o/r?depth=1#{c}"),
        format!("git+https://codeberg.org/o/r?ref=a&x=1#{c}"),
        format!("git+https://codeberg.org/o/../x#{c}"),
        format!("git+https://codeberg.org/o/r/extra#{c}"),
        format!("git+https://codeberg.org/o r#{c}"),
        format!("git+http://codeberg.org/o/r#{c}"),
        format!("git+ssh://codeberg.org/o/r#{c}"),
        "http://codeberg.org/o/r/archive/v1.tar.gz".to_string(),
    ];
    for url in refused {
        assert!(source_of(&url).is_err(), "{url} should be refused");
    }
}

#[test]
fn a_git_url_on_another_host_is_a_commit_to_fetch_with_git() {
    let c = HEX40;
    let cases = [
        format!("git+https://gitlab.freedesktop.org/o/r#{c}"),
        format!("git+https://example.org/o/r?ref=main#{c}"),
        // Without a `~user` owner there is no sr.ht archive URL.
        format!("git+https://git.sr.ht/o/r#{c}"),
    ];
    for url in cases {
        match source_of(&url).unwrap() {
            Source::Git(git) => assert_eq!(git.commit, c),
            other => panic!("{url} is not a git source: {other:?}"),
        }
    }
}

#[test]
fn a_git_url_without_a_commit_is_refused_with_the_reason() {
    let err = source_of("git+https://example.org/o/r?ref=main").unwrap_err();
    assert!(err.contains("has no #commit"), "{err}");
}
