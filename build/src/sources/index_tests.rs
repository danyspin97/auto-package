use super::*;

#[test]
pub(super) fn index_links_keep_only_file_names() {
    let html = r##"<a href="?C=N;O=D">Name</a> <a href="../">Parent</a>
            <a href='/releases/lzip/lzip-1.24.tar.gz'>x</a>
            <A HREF="lzip-1.24.tar.gz.sig">sig</A>
            <a href="lzip-1.24.tar.gz">dup</a><a href="#top">t</a>"##;
    assert_eq!(
        parse_index_links(html),
        ["lzip-1.24.tar.gz", "lzip-1.24.tar.gz.sig"]
    );
}

#[test]
pub(super) fn index_links_of_a_large_listing_parse_quickly() {
    let html: String = (0..20_000)
        .map(|i| format!("<a href=\"pkg-{i}.tar.gz\">pkg-{i}</a>\n"))
        .collect();
    let t = std::time::Instant::now();
    assert_eq!(parse_index_links(&html).len(), 20_000);
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
pub(super) fn index_archives_sort_numerically_and_demote_prereleases() {
    let links: Vec<String> = [
        "foo-1.9.tar.gz",
        "foo-1.10.tar.gz",
        "foo-2.0rc1.tar.gz",
        "foo-1.10.tar.xz",
        "foo-1.10.tar.gz.sig",
        "other-9.9.tar.gz",
    ]
    .map(String::from)
    .to_vec();
    let got = index_archives(&links, "foo");
    let versions: Vec<_> = got.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(versions, ["1.10", "1.9", "2.0rc1"]);
    assert_eq!(got[0].0, "foo-1.10.tar.gz");
}

#[test]
pub(super) fn a_hint_that_matches_nothing_falls_back_to_every_tarball() {
    let links: Vec<String> = ["bar-1.0.tar.gz".to_string()].to_vec();
    assert_eq!(index_archives(&links, "foo").len(), 1);
}

#[test]
pub(super) fn xz_only_index_is_reported_as_unsupported_not_empty() {
    let links: Vec<String> = ["foo-1.0.tar.xz".to_string()].to_vec();
    assert!(index_archives(&links, "foo").is_empty());
    assert!(index_has_only_unsupported(&links));
    assert!(!index_has_only_unsupported(&["x.html".to_string()]));
}

#[test]
pub(super) fn direct_archive_resolves_without_any_network() {
    let u = "https://example.org/pub/foo-3.1.4.tar.gz";
    assert_eq!(latest_release(u).unwrap(), "3.1.4");
    assert_eq!(archive_urls(u, "3.1.4"), [u]);
}

#[test]
pub(super) fn direct_archive_without_a_parsable_version_must_name_the_requested_one() {
    let v1 = "https://github.com/o/r/archive/v1.0.tar.gz";
    // A `v1.0` name cannot serve 2.0, and no URL is returned for it.
    assert!(archive_urls(v1, "2.0").is_empty());
    // The same name serves 1.0, with or without the leading `v`.
    assert_eq!(archive_urls(v1, "1.0"), [v1]);
    assert_eq!(archive_urls(v1, "v1.0"), [v1]);
    // A name with a parsable version is still checked the same way.
    let foo = "https://github.com/o/r/archive/foo-1.0.tar.gz";
    assert_eq!(archive_urls(foo, "1.0"), [foo]);
    assert!(archive_urls(foo, "2.0").is_empty());
    // An unversioned name does not name 2.0 either.
    let bare = "https://github.com/o/r/archive/source.tar.gz";
    assert!(archive_urls(bare, "2.0").is_empty());
}

#[test]
pub(super) fn file_names_version_needs_whole_version_tokens() {
    assert!(file_names_version("v1.0.tar.gz", "1.0"));
    assert!(file_names_version("v1.0.zip", "1.0"));
    assert!(file_names_version("release-1.0.tar.gz", "1.0"));
    assert!(!file_names_version("v1.0.tar.gz", "2.0"));
    assert!(!file_names_version("v1.0.5.tar.gz", "1.0"));
    assert!(!file_names_version("x1.0.tar.gz", "1.0"));
    assert!(!file_names_version("v1.0", "1.0"), "no archive extension");
}

#[test]
pub(super) fn a_direct_archive_without_a_parsed_version_ends_its_version_at_the_name() {
    // `-docs`/`-data` after the version is another file, not this release.
    let docs = "https://github.com/o/r/archive/v1.0-docs.tar.gz";
    assert!(archive_urls(docs, "1.0").is_empty());
    let data = "https://github.com/o/r/archive/v1.0_data.tar.gz";
    assert!(archive_urls(data, "1.0").is_empty());
    // The name that ends at the version is the release.
    let plain = "https://github.com/o/r/archive/v1.0.tar.gz";
    assert_eq!(archive_urls(plain, "1.0"), [plain]);
    // A wheel tag may follow it; an unversioned name names no version.
    assert!(file_names_version("foo-v1.0-py3-none-any.whl", "1.0"));
    assert!(file_names_version("v1.0-1-py3-none-any.whl", "1.0"));
    assert!(!file_names_version("v1.0-docs-x.whl", "1.0"));
    assert!(file_names_version("v1.0.tar", "1.0"));
    let bare = "https://github.com/o/r/archive/source.tar.gz";
    assert!(archive_urls(bare, "2.0").is_empty());
    assert!(!file_names_version("v1.0-docs.tar.gz", "1.0"));
}

#[test]
pub(super) fn a_wheel_tag_suffix_must_have_the_wheel_tag_shape() {
    // A name that is not a version plus wheel tags is another file.
    assert!(!file_names_version("v1.0-docs-html-pdf.tar.gz", "1.0"));
    assert!(!file_names_version("v1.0-docs.tar.gz", "1.0"));
    assert!(!file_names_version("v1.0-docs-html-pdf.whl", "1.0"));
    assert!(
        !file_names_version("v1.0-x-y-z.whl", "1.0"),
        "python tag is not one"
    );
    // The wheel tags of a real wheel: python, ABI and platform.
    assert!(file_names_version("1.0-py3-none-any.whl", "1.0"));
    assert!(file_names_version("1.0-1-py3-none-any.whl", "1.0"));
    assert!(file_names_version("1.0-py2.py3-none-any.whl", "1.0"));
    assert!(file_names_version(
        "1.0-cp311-abi3-manylinux_2_17_x86_64.whl",
        "1.0"
    ));
    assert!(
        !file_names_version("1.0-py3-none.whl", "1.0"),
        "two tags, not three"
    );
    assert!(
        !file_names_version("1.0-x1-py3-none-any.whl", "1.0"),
        "build tag is digits"
    );
}

#[test]
pub(super) fn a_self_hosted_gitlab_gets_the_gitlab_archive_layout() {
    for gitlab in [
        "https://gitlab.com/g/r",
        "https://git.gitlab.corp.example/team/libfoo",
        "https://gitlab-internal.corp.example/team/libfoo",
    ] {
        assert!(url_is_gitlab(gitlab), "{gitlab}");
    }
    for other in ["https://github.com/o/r", "https://evil-gitlab.example/x/y"] {
        assert!(!url_is_gitlab(other), "{other}");
    }
    let base = "https://git.gitlab.corp.example/team/libfoo";
    assert_eq!(
        tag_archive_url_for(base, "libfoo", url_is_gitlab(base), "v1.0"),
        "https://git.gitlab.corp.example/team/libfoo/-/archive/v1.0/libfoo-v1.0.tar.gz"
    );
    let base = "https://gitlab-internal.corp.example/team/libfoo";
    assert_eq!(
        tag_archive_url_for(base, "libfoo", url_is_gitlab(base), "v1.0"),
        "https://gitlab-internal.corp.example/team/libfoo/-/archive/v1.0/libfoo-v1.0.tar.gz"
    );
}

/// Live forge tag lookup: public, unauthenticated GETs of api.github.com and
/// gitlab.com. Run with `cargo test -- --ignored`, and without `gh` on PATH
/// (which would send the user's token) when that matters.
#[test]
#[ignore = "network: GETs the GitHub and GitLab APIs"]
pub(super) fn live_tag_archive_url_finds_a_real_tag() {
    // A tag that has existed for years, spelled with and without a `v`.
    assert_eq!(
        tag_archive_url("https://github.com/pallets/flask", "3.0.0").as_deref(),
        Some("https://github.com/pallets/flask/archive/refs/tags/3.0.0.tar.gz")
    );
    assert_eq!(
        tag_archive_url("https://github.com/psf/requests", "2.31.0").as_deref(),
        Some("https://github.com/psf/requests/archive/refs/tags/v2.31.0.tar.gz")
    );
    // No such version: no guess.
    assert_eq!(
        tag_archive_url("https://github.com/pallets/flask", "0.0.0.0.1"),
        None
    );
    // GitLab's layout (`-/archive/<tag>/<repo>-<tag>.tar.gz`): whichever tag
    // the API lists first must be found again by its own version.
    let repo = "https://gitlab.com/gitlab-org/cli";
    let tags = get_repo_tags(repo).unwrap();
    let tag = tags.first().expect("gitlab-org/cli has tags");
    let version = version_from_tag(tag, "cli");
    let url = tag_archive_url(repo, &version).unwrap_or_else(|| panic!("{tag} -> {version}"));
    assert!(
        url.starts_with("https://gitlab.com/gitlab-org/cli/-/archive/"),
        "{url}"
    );
    assert!(url.ends_with(".tar.gz"), "{url}");
    // The URL it hands out exists (GET, headers only: the body is dropped).
    let resp = crate::http::agent(crate::http::API_TIMEOUT, 5)
        .get(&url)
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "{url}");
}

#[test]
pub(super) fn version_cmp_orders_numbers_letters_and_markers() {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let table = [
        ("1.10", "1.9", Greater),
        ("1.2.3.4", "1.2.3", Greater),
        ("1.2.3a", "1.2.3", Greater),
        ("1.2.3a", "1.10", Less),
        ("1.0rc1", "1.0", Less),
        ("1.0-beta2", "1.0-beta10", Less),
        ("1.0~git20260101", "1.0", Less),
        ("1.0+git20260101", "1.0", Greater),
        ("1.0.1", "1.0rc2", Greater),
        ("007", "7", Equal),
        ("1.0", "1.0", Equal),
    ];
    for (a, b, want) in table {
        assert_eq!(version_cmp(a, b), want, "{a} vs {b}");
    }
}

#[test]
pub(super) fn prerelease_words_are_whole_words() {
    for v in [
        "1.0rc1",
        "2.0-beta2",
        "1.3.12-pre3",
        "1.0~git",
        "3.0.dev1",
        "1.0+git20260101",
    ] {
        assert!(is_prerelease(v), "{v}");
    }
    for v in [
        "1.2.3",
        "4.3.8.src",
        "1.2.3a",
        "2.1.13-stable",
        "1.0-prefix",
        "1.0.arch",
    ] {
        assert!(!is_prerelease(v), "{v}");
    }
}

#[test]
pub(super) fn release_versions_drop_source_markers_and_reject_builds() {
    assert_eq!(release_version("3.9.5-src").as_deref(), Some("3.9.5"));
    assert_eq!(release_version("4.3.8.src").as_deref(), Some("4.3.8"));
    assert_eq!(release_version("1.10-win32-dll-openssl"), None);
    assert_eq!(
        release_version("1.0.0-beta2").as_deref(),
        Some("1.0.0-beta2")
    );
    assert_eq!(release_version("latest"), None);
}
