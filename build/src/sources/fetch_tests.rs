use super::*;

use super::helpers::*;

#[test]
pub(super) fn a_repository_that_moved_to_another_name_is_recognised() {
    let moved: serde_json::Value = serde_json::from_str(
            r#"{"tag_name":"v3.16.1","tarball_url":"https://api.github.com/repos/catchorg/Catch2/tarball/v3.16.1"}"#,
        )
        .unwrap();
    assert_eq!(answered_repo_name(&moved).as_deref(), Some("Catch2"));
    let list: serde_json::Value = serde_json::from_str(
        r#"[{"name":"v1","tarball_url":"https://api.github.com/repos/o/Same/tarball/v1"}]"#,
    )
    .unwrap();
    assert_eq!(answered_repo_name(&list).as_deref(), Some("Same"));
    assert_eq!(
        answered_repo_name(&serde_json::json!({"tag_name": "v1"})),
        None
    );
}

#[test]
pub(super) fn tag_archive_urls_follow_the_forge_layout() {
    assert_eq!(
        tag_archive_url_for("https://github.com/o/r", "r", false, "release/1.0"),
        "https://github.com/o/r/archive/refs/tags/release/1.0.tar.gz"
    );
    assert_eq!(
        tag_archive_url_for("https://gitlab.com/g/r", "r", true, "v1 2"),
        "https://gitlab.com/g/r/-/archive/v1%202/r-v1%202.tar.gz"
    );
}

/// A dashed tag is found by the RPM version it is packaged under (`v2.9.0-1`
/// as `2.9.0.1`), and its archive is the tag's own, not the RPM version's.
#[test]
pub(super) fn a_dashed_tag_is_found_by_its_rpm_version() {
    let louvre = ["v2.8.0-1", "v2.9.0-1"].map(String::from);
    assert_eq!(
        find_matching_tag(&louvre, "2.9.0.1", "Louvre").as_deref(),
        Some("v2.9.0-1")
    );
    let srm = ["v0.8.0-1", "v0.8.0-2"].map(String::from);
    assert_eq!(
        find_matching_tag(&srm, "0.8.0.2", "SRM").as_deref(),
        Some("v0.8.0-2")
    );
    assert_eq!(find_matching_tag(&srm, "0.8.0.3", "SRM"), None);
    let tag = find_matching_tag(&srm, "0.8.0.2", "SRM").unwrap();
    assert_eq!(
        tag_archive_url_for("https://github.com/CuarzoSoftware/SRM", "SRM", false, &tag),
        "https://github.com/CuarzoSoftware/SRM/archive/refs/tags/v0.8.0-2.tar.gz"
    );
}

/// An exact tag beats a dashed tag that maps to the same version, and two that
/// map to one version give the same tag in either order.
#[test]
pub(super) fn an_exact_tag_is_preferred_to_a_mapped_one() {
    for tags in [["v1.0.0-1", "v1.0.0.1"], ["v1.0.0.1", "v1.0.0-1"]] {
        let tags = tags.map(String::from);
        assert_eq!(
            find_matching_tag(&tags, "1.0.0.1", "Louvre").as_deref(),
            Some("v1.0.0.1")
        );
    }
    for tags in [
        ["v1.0.0-1", "release-1.0.0-1"],
        ["release-1.0.0-1", "v1.0.0-1"],
    ] {
        let tags = tags.map(String::from);
        assert_eq!(
            find_matching_tag(&tags, "1.0.0.1", "Louvre").as_deref(),
            Some("release-1.0.0-1")
        );
    }
}

#[test]
pub(super) fn gh_api_only_takes_the_requests_this_crate_makes() {
    for ok in [
        "repos/o/r/releases/latest",
        "repos/o/r/releases?per_page=30",
        "repos/o/r/tags?per_page=100",
        "search/repositories?q=foo%20bar&sort=stars&order=desc",
        "search/commits?q=CVE-2025-1234+repo%3Ao%2Fr",
        "repos/o/r/compare/v1.2...0123abc",
        "repos/o/r/pulls/106",
    ] {
        assert!(gh_api_path_ok(ok), "{ok}");
    }
    for bad in [
        "--hostname evil.example",
        "-X",
        "user",
        "repos/o/r/contents/x",
        "repos/o/r/compare/v1.2",
        "repos/o/r/pulls/106/files",
        "repos/o/r/pulls/x",
        "search/code?q=x",
        "search/commits -X DELETE",
        "repos/o/../r/tags",
        "repos/o/r/tags?x=1 -X DELETE",
        "graphql",
    ] {
        assert!(!gh_api_path_ok(bad), "{bad}");
    }
}

#[test]
pub(super) fn name_normalisation_is_pep_503() {
    for n in ["foo.bar", "foo--bar", "Foo_Bar", "FOO-._bar"] {
        assert_eq!(super::normalize_name(n), "foo-bar", "{n}");
    }
    let mut m = super::SourceMap::default();
    m.insert("Foo_Bar", "https://github.com/o/r");
    assert!(m.get("foo.bar").is_some() && m.get("foo--bar").is_some());
}

#[test]
pub(super) fn version_from_tag_survives_non_ascii_tags() {
    assert_eq!(super::version_from_tag("réléase-1", "foo"), "réléase-1");
    assert_eq!(super::version_from_tag("fé", "foo"), "fé");
    assert_eq!(super::version_from_tag("foo-v1.2", "foo"), "1.2");
}

#[test]
pub(super) fn forge_archive_urls_are_forge_not_direct() {
    // A fixed archive file is what the URL names; only repository and tag
    // URLs go through the forge.
    assert_eq!(
        super::source_kind("https://github.com/o/r/archive/v1.0.tar.gz"),
        super::SourceKind::DirectArchive
    );
    assert_eq!(
        super::source_kind("https://github.com/o/r/tree/v1.0"),
        super::SourceKind::Forge
    );
}

#[test]
pub(super) fn direct_archive_must_match_requested_version() {
    let url = "https://example.org/dl/foo-3.1.4.tar.gz";
    let kind = super::SourceKind::DirectArchive;
    assert!(super::archive_urls_non_forge(url, kind, "3.2.0").is_empty());
    assert_eq!(super::archive_urls_non_forge(url, kind, "3.1.4"), vec![url]);
    // A name with no parsable version is trusted only if it names the
    // requested one.
    let bare = "https://example.org/dl/latest.tar.gz";
    assert!(super::archive_urls_non_forge(bare, kind, "3.2.0").is_empty());
    let named = "https://example.org/dl/latest-3.2.0.tar.gz";
    assert_eq!(
        super::archive_urls_non_forge(named, kind, "3.2.0"),
        vec![named]
    );
}

#[test]
pub(super) fn redirects_may_not_downgrade_to_http() {
    let from = "https://api.example.org/x";
    assert!(super::redirects::redirect_target(from, "http://evil.example/x").is_err());
    assert!(super::redirects::redirect_target(from, "HTTP://evil.example/x").is_err());
    assert_eq!(
        super::redirects::redirect_target(from, "https://b.example/y").unwrap(),
        "https://b.example/y"
    );
    assert_eq!(
        super::redirects::redirect_target(from, "/z").unwrap(),
        "https://api.example.org/z"
    );
}

#[test]
pub(super) fn a_recent_failure_is_reused_instead_of_downloading_again() {
    // Unique URL, never requested: a download attempt would return a
    // transport error with a different message.
    let url = "https://invalid.example/recent-failure-test.tar.gz";
    archive_cache().lock().unwrap().failed.insert(
        url.to_string(),
        (std::time::Instant::now(), FetchFailure::RateLimited),
    );
    assert_eq!(fetch_archive(url), Err(FetchFailure::RateLimited));
}

#[test]
pub(super) fn a_fetched_archive_is_moved_to_its_destination_without_a_download() {
    // The probe's copy is handed over, not fetched again. The URLs are
    // unique to this test and never requested.
    let dir = tempfile::tempdir().unwrap();
    let cached = dir.path().join("cached.tar.gz");
    std::fs::write(&cached, b"archive").unwrap();
    let url = "https://example.invalid/zz-handover-1.0.tar.gz";
    archive_cache()
        .lock()
        .unwrap()
        .files
        .insert(url.to_string(), ArchiveFile::new(cached.clone()));
    let dest = dir.path().join("out.tar.gz");
    fetch_archive_to(url, &dest).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"archive");
    assert!(!cached.exists());

    let gone = "https://example.invalid/zz-handover-missing-1.0.tar.gz";
    archive_cache()
        .lock()
        .unwrap()
        .missing
        .insert(gone.to_string());
    assert_eq!(
        fetch_archive_to(gone, &dest).unwrap_err(),
        FetchFailure::NotFound
    );
}

/// User-reported Oct 7 regression: `could not work out the latest
/// version of https://github.com/drogonframework/drogon; ...
/// api.github.com/.../tags?per_page=1: http status: 403`.
///
/// 403 on the anon GitHub API means the IP has blown through the
/// 60-req/hour cap. Error must mention the GITHUB_TOKEN remedy so
/// the user doesn't have to go read the GitHub docs.
#[test]
pub(super) fn describe_forge_error_names_the_github_token_remedy_on_403() {
    // Make sure neither env var is set so the "no auth, show the fix"
    // branch runs — tests run in parallel, so take a stable snapshot
    // of the env and restore it at the end (never panic across the
    // unsafe restore).
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved_gh = std::env::var("GH_TOKEN").ok();
    let saved_gh_token = std::env::var("GITHUB_TOKEN").ok();
    // SAFETY: single-threaded within this test; restored below.
    unsafe {
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
    }

    let msg = describe_forge_http_error(
        "https://api.github.com/repos/drogonframework/drogon/tags?per_page=1",
        &ForgeError::Status(403),
    );

    // Restore before any assertion that might panic.
    unsafe {
        if let Some(v) = saved_gh {
            std::env::set_var("GH_TOKEN", v);
        }
        if let Some(v) = saved_gh_token {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    assert!(
        msg.contains("GITHUB_TOKEN") && msg.contains("60 req/hour"),
        "403 must point the user at GITHUB_TOKEN and name the rate limit; got {msg:?}"
    );
}

/// With a token present, 403 means the token itself is being
/// rate-limited (abuse detection, hitting the 5000/hour ceiling, or
/// a secondary rate limit) — the fix is different, so the message
/// must be different.
#[test]
pub(super) fn describe_forge_error_hints_rotate_token_when_authed() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var("GITHUB_TOKEN").ok();
    unsafe {
        std::env::set_var("GITHUB_TOKEN", "ghp_fake");
    }

    let msg = describe_forge_http_error(
        "https://api.github.com/repos/x/y/releases/latest",
        &ForgeError::Status(403),
    );

    unsafe {
        match saved {
            Some(v) => std::env::set_var("GITHUB_TOKEN", v),
            None => std::env::remove_var("GITHUB_TOKEN"),
        }
    }

    assert!(
        msg.contains("rate-limited") && msg.contains("rotate"),
        "authed 403 must suggest wait/rotate; got {msg:?}"
    );
}

#[test]
pub(super) fn describe_forge_error_leaves_non_auth_errors_alone() {
    let msg = describe_forge_http_error(
        "https://api.github.com/repos/x/y/tags?per_page=1",
        &ForgeError::Status(500),
    );
    assert!(
        msg.contains("500") && !msg.contains("GITHUB_TOKEN"),
        "non-auth errors stay a raw passthrough; got {msg:?}"
    );
}

#[test]
pub(super) fn forge_errors_are_typed_from_the_status_and_headers() {
    use ap_ecosystem::net::{ReplyHeaders, StatusFailure};
    let fail = |status, remaining, retry_after| StatusFailure {
        status,
        headers: ReplyHeaders {
            retry_after,
            remaining,
            reset: None,
        },
    };
    assert_eq!(
        ForgeError::from_status_failure(&fail(429, None, Some(30))),
        ForgeError::RateLimited {
            status: 429,
            retry_after: Some(30)
        }
    );
    assert_eq!(
        ForgeError::from_status_failure(&fail(403, Some(0), None)),
        ForgeError::RateLimited {
            status: 403,
            retry_after: None
        }
    );
    assert_eq!(
        ForgeError::from_status_failure(&fail(403, Some(7), None)),
        ForgeError::Status(403)
    );
    assert_eq!(ForgeError::from_status(404), ForgeError::NotFound);
    assert_eq!(ForgeError::from_status(401), ForgeError::Unauthorized);
    assert_eq!(
        ForgeError::from_ureq(&ureq::Error::StatusCode(502)),
        ForgeError::Status(502)
    );
    assert!(matches!(
        ForgeError::from_ureq(&ureq::Error::HostNotFound),
        ForgeError::Transport(_)
    ));
}

#[test]
pub(super) fn a_transport_message_that_mentions_a_status_is_not_a_rate_limit() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let msg = describe_forge_http_error(
        "https://gitlab.com/api/v4/projects/x",
        &ForgeError::Transport("proxy said status: 403".into()),
    );
    assert!(!msg.contains("rate-limited"), "{msg}");
}

#[test]
pub(super) fn github_is_decided_by_the_host_not_a_substring() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = (
        std::env::var("GH_TOKEN").ok(),
        std::env::var("GITHUB_TOKEN").ok(),
    );
    // SAFETY: serialised by ENV_LOCK; restored below.
    unsafe {
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
    }
    let limited = ForgeError::RateLimited {
        status: 429,
        retry_after: Some(5),
    };
    let real = describe_forge_http_error("https://api.github.com/repos/x/y", &limited);
    let fake = describe_forge_http_error("https://evil.example/api.github.com/x", &limited);
    unsafe {
        if let Some(v) = saved.0 {
            std::env::set_var("GH_TOKEN", v);
        }
        if let Some(v) = saved.1 {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }
    assert!(
        real.contains("60 req/hour") && real.contains("retry after 5s"),
        "{real}"
    );
    assert!(!fake.contains("60 req/hour"), "{fake}");
}

#[test]
pub(super) fn a_path_handed_out_survives_the_handover_to_a_destination() {
    // A thread holds the probe's copy while another takes it for the
    // checkout: the first path must still be readable afterwards.
    let dir = tempfile::tempdir().unwrap();
    let cached = dir.path().join("cached.tar.gz");
    std::fs::write(&cached, b"archive").unwrap();
    let url = "https://example.invalid/zz-lease-1.0.tar.gz";
    let lease = ArchiveFile::new(cached.clone());
    archive_cache()
        .lock()
        .unwrap()
        .files
        .insert(url.to_string(), Arc::clone(&lease));
    let held = fetch_archive(url).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (b2, dest) = (barrier.clone(), dir.path().join("out.tar.gz"));
    let t = std::thread::spawn({
        let dest = dest.clone();
        move || {
            b2.wait();
            fetch_archive_to(url, &dest).unwrap();
            b2.wait();
        }
    });
    barrier.wait();
    barrier.wait();
    t.join().unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"archive");
    assert_eq!(std::fs::read(&*held).unwrap(), b"archive");
    // Nobody can fetch it from the cache any more, and the entry is gone.
    assert!(!archive_cache().lock().unwrap().files.contains_key(url));
    // The file goes when the last lease does.
    drop((held, lease));
    assert!(!cached.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"archive");
}

#[test]
pub(super) fn an_unshared_archive_is_moved_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let cached = dir.path().join("only.tar.gz");
    std::fs::write(&cached, b"x").unwrap();
    let url = "https://example.invalid/zz-lease-sole-1.0.tar.gz";
    archive_cache()
        .lock()
        .unwrap()
        .files
        .insert(url.to_string(), ArchiveFile::new(cached.clone()));
    let dest = dir.path().join("dest.tar.gz");
    fetch_archive_to(url, &dest).unwrap();
    assert!(!cached.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"x");
}

#[test]
pub(super) fn evicting_keeps_the_file_for_whoever_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e.tar.gz");
    std::fs::write(&path, b"x").unwrap();
    let url = "https://example.invalid/zz-evict-1.0.tar.gz";
    let file = ArchiveFile::new(path.clone());
    archive_cache()
        .lock()
        .unwrap()
        .files
        .insert(url.to_string(), Arc::clone(&file));
    evict_archive(url);
    assert!(path.exists());
    drop(file);
    assert!(!path.exists());
}

#[test]
pub(super) fn in_flight_slots_are_serialised_and_removed_when_idle() {
    let url = "https://example.invalid/zz-slot-1.0.tar.gz";
    let first = Slot::take(url);
    let order = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(std::sync::Barrier::new(2));
    let waiter = std::thread::spawn({
        let (order, started) = (order.clone(), started.clone());
        move || {
            started.wait();
            let _second = Slot::take(url);
            order.lock().unwrap().push("second");
        }
    });
    started.wait();
    // The waiter cannot have the turn while `first` is held.
    order.lock().unwrap().push("first");
    drop(first);
    waiter.join().unwrap();
    assert_eq!(*order.lock().unwrap(), ["first", "second"]);
    assert!(!archive_cache().lock().unwrap().in_flight.contains_key(url));
}

#[test]
pub(super) fn lookup_ignores_name_spelling() {
    let mut m = SourceMap::default();
    m.insert("nvidia-ml-py", "https://github.com/nvidia/nvidia-ml-py");
    assert!(m.get("nvidia_ml_py").is_some());
    assert!(m.get("NVIDIA-ML-PY").is_some());
    assert!(m.get("nvidia-ml").is_none());
}

#[test]
pub(super) fn a_bare_url_applies_to_the_package_being_built() {
    let (pkg, url) = SourceMap::parse_arg("https://github.com/x/y", "nvidia-cuda-runtime").unwrap();
    assert_eq!(pkg, "nvidia-cuda-runtime");
    assert_eq!(url, "https://github.com/x/y");
}

#[test]
pub(super) fn an_explicit_package_wins_over_the_default() {
    let (pkg, url) = SourceMap::parse_arg("cuda-python=https://github.com/x/y", "root").unwrap();
    assert_eq!(pkg, "cuda-python");
    assert_eq!(url, "https://github.com/x/y");
}

#[test]
pub(super) fn a_value_that_is_neither_is_rejected_rather_than_guessed() {
    assert!(SourceMap::parse_arg("just-a-name", "root").is_err());
    assert!(SourceMap::parse_arg("pkg=", "root").is_err());
}
