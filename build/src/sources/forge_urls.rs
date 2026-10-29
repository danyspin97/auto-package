use super::*;

use super::helpers::*;

#[test]
pub(super) fn gitlab_uses_its_own_archive_layout() {
    let urls = archive_urls("https://gitlab.com/owner/thing", "1.2.3");
    assert!(
        urls[0].contains("/-/archive/v1.2.3/thing-v1.2.3.tar.gz"),
        "{urls:?}"
    );

    let urls = archive_urls("https://github.com/owner/thing.git", "1.2.3");
    assert_eq!(
        urls[0],
        "https://github.com/owner/thing/archive/refs/tags/v1.2.3.tar.gz"
    );
}

#[test]
pub(super) fn both_url_spellings_split_the_same_way() {
    for url in [
        "https://github.com/NVIDIA/cuda-python",
        "https://github.com/NVIDIA/cuda-python.git",
        "https://github.com/NVIDIA/cuda-python/",
        "git@github.com:NVIDIA/cuda-python.git",
    ] {
        let (host, project_path, repo) = owner_repo(url).unwrap();
        assert_eq!(
            (host.as_str(), project_path.as_str(), repo.as_str()),
            ("github.com", "NVIDIA/cuda-python", "cuda-python"),
            "{url}",
        );
    }
    assert!(owner_repo("https://github.com/NVIDIA").is_err());
}

#[test]
pub(super) fn gitlab_nested_group_is_preserved_through_owner_repo() {
    // The whole project path survives — `group/subgroup/project`, not
    // just the first two segments. latest_release's GitLab branch then
    // URL-encodes it correctly for the API.
    let (host, project_path, repo) =
        owner_repo("https://gitlab.com/group/subgroup/project").unwrap();
    assert_eq!(host, "gitlab.com");
    assert_eq!(project_path, "group/subgroup/project");
    assert_eq!(repo, "project");
}

#[test]
pub(super) fn ssh_scp_style_normalises_before_api_calls() {
    // Even the SSH spelling resolves to the same (host, project_path,
    // repo) — downstream HTTPS API calls work transparently.
    let (host, project_path, _) = owner_repo("git@gitlab.com:group/subgroup/project.git").unwrap();
    assert_eq!(host, "gitlab.com");
    assert_eq!(project_path, "group/subgroup/project");
}

/// Owner and repository become path segments of API and archive URLs, so
/// traversal, escapes and query characters are refused.
#[test]
pub(super) fn owner_and_repo_must_be_plain_names() {
    for url in [
        "https://github.com/../orgs",
        "https://github.com/owner/..",
        "https://github.com/owner/.",
        "https://github.com/a%2Fb/repo",
        "https://github.com/owner/repo%2e%2e",
        "https://github.com/group/../repo",
    ] {
        assert!(owner_repo(url).is_err(), "{url} should be refused");
    }
    assert!(owner_repo("https://github.com/owner/repo.name_1-x").is_ok());
    assert!(owner_repo("https://gitlab.com/group/subgroup/project").is_ok());
}

#[test]
pub(super) fn a_tag_is_reduced_to_the_version_it_carries() {
    assert_eq!(version_from_tag("v12.9.0", "cuda-python"), "12.9.0");
    assert_eq!(version_from_tag("12.9.0", "cuda-python"), "12.9.0");
    assert_eq!(
        version_from_tag("cuda-python-v12.9.0", "cuda-python"),
        "12.9.0"
    );
    assert_eq!(version_from_tag("release-1.2", "thing"), "1.2");
    // Not a version prefix: a tag that merely starts with a letter v.
    assert_eq!(version_from_tag("voyager", "thing"), "voyager");
}

#[test]
pub(super) fn a_resolved_version_round_trips_back_to_its_tag() {
    let tag = "v12.9.0";
    let version = version_from_tag(tag, "cuda-python");
    let urls = archive_urls("https://github.com/NVIDIA/cuda-python", &version);
    assert!(urls[0].ends_with(&format!("/{tag}.tar.gz")), "{urls:?}");
}

#[test]
pub(super) fn a_forge_answer_yields_its_tag_whether_wrapped_in_an_array_or_not() {
    let one: serde_json::Value = serde_json::json!({"tag_name": "v1.0.0"});
    assert_eq!(first_tag(&one).as_deref(), Some("v1.0.0"));
    let many: serde_json::Value = serde_json::json!([{"name": "v2.0.0"}, {"name": "v1.0.0"}]);
    assert_eq!(first_tag(&many).as_deref(), Some("v2.0.0"));
    assert_eq!(first_tag(&serde_json::json!([])), None);
}

#[test]
pub(super) fn rust_crates_are_not_given_the_python_prefix() {
    assert_eq!(Ecosystem::Rust.rpm_name("ripgrep"), "ripgrep");
    assert_eq!(
        Ecosystem::Python.rpm_name("MarkupSafe"),
        "python-markupsafe"
    );
    assert_eq!(Ecosystem::PlainC.rpm_name("nlohmann-json"), "nlohmann-json");
}

#[test]
pub(super) fn round_trips_through_toml() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = SourceMap::default();
    m.insert("cuda-python", "https://github.com/NVIDIA/cuda-python");
    m.save(dir.path()).unwrap();
    let back = SourceMap::load(dir.path()).unwrap();
    assert_eq!(
        back.get("cuda_python"),
        Some("https://github.com/NVIDIA/cuda-python")
    );
}

#[test]
pub(super) fn a_missing_file_is_an_empty_map_but_a_broken_one_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(SourceMap::load(dir.path()).unwrap().is_empty());
    std::fs::write(SourceMap::path(dir.path()), "this is not toml {{{").unwrap();
    assert!(SourceMap::load(dir.path()).is_err());
}

/// Baseline `SdistInfo` with all layout booleans cleared — build it
/// up per-test by toggling the fields the scenario depends on, so
/// the test reads as "project X with layout Y" instead of a wall of
/// `..Default::default()` boilerplate.
pub(super) fn bare_info() -> ap_ecosystem::sdist::SdistInfo {
    ap_ecosystem::sdist::SdistInfo {
        project_name: None,
        project_dir: None,
        top_level_dir: None,
        subprojects: Vec::new(),
        build_system: ap_ecosystem::sdist::BuildSystem {
            requires: Vec::new(),
            backend: None,
        },
        python_packaging: false,
        cargo_manifest: false,
        cmake_manifest: false,
        autotools_manifest: false,
        cmake_at_root: false,
        autotools_at_root: false,
        has_c_extensions: false,
        has_tests: false,
        top_level_tests_dir: None,
        files: Vec::new(),
        console_scripts: Vec::new(),
    }
}

/// The llama.cpp motivator: `CMakeLists.txt` + `pyproject.toml` at
/// the root, pyproject declares `llama-cpp-scripts`, user asked for
/// `llama-cpp`. Must flip the ecosystem choice to C/C++ instead of
/// silently renaming through to the Python stub.
#[test]
pub(super) fn python_hijack_fires_when_cmake_and_a_different_pyproject_name_coexist() {
    let mut info = bare_info();
    info.python_packaging = true;
    info.cmake_at_root = true;
    info.project_name = Some("llama-cpp-scripts".to_string());
    assert!(
        is_python_hijack(&info, "llama-cpp"),
        "CMake + pyproject with a different declared name must be \
             treated as a C/C++ project with a Python-helper sidecar"
    );
}

/// Same decision on autotools, so the fix is not CMake-only.
/// google/re2: CMake at the root, a Python wrapper under `python/` whose
/// setup.py declares no pyproject name. The C++ library is the project.
#[test]
pub(super) fn python_hijack_fires_for_a_nameless_python_binding_in_a_subdirectory() {
    let mut info = bare_info();
    info.python_packaging = true;
    info.cmake_at_root = true;
    info.project_dir = Some("python".to_string());
    assert!(is_python_hijack(&info, "re2"));
    // ...but nameless Python packaging AT the root stays Python.
    info.project_dir = None;
    assert!(!is_python_hijack(&info, "re2"));
}

#[test]
pub(super) fn python_hijack_also_fires_on_autotools_plus_pyproject() {
    let mut info = bare_info();
    info.python_packaging = true;
    info.autotools_at_root = true;
    info.project_name = Some("some-tool-scripts".to_string());
    assert!(is_python_hijack(&info, "some-tool"));
}

/// A maturin project (pyproject + Cargo.toml, no CMake) must NOT
/// be flipped — it is a Python package that compiles Rust, and the
/// Python side is primary. This is the invariant the original
/// comment at `probe` was protecting; the fix must preserve it.
#[test]
pub(super) fn python_hijack_does_not_fire_on_maturin_projects() {
    let mut info = bare_info();
    info.python_packaging = true;
    info.cargo_manifest = true;
    info.cmake_at_root = true;
    info.project_name = Some("totally-different-name".to_string());
    assert!(
        !is_python_hijack(&info, "pydantic-core"),
        "a maturin project (pyproject + Cargo + maybe CMake) must \
             stay Python-primary even when its declared name differs"
    );
}

/// A Python-only project (no CMake, no autotools) must still rename
/// through its declared name — the Flask / Pallets case the original
/// comment at `probe` was built for.
#[test]
pub(super) fn python_hijack_does_not_fire_without_a_native_manifest() {
    let mut info = bare_info();
    info.python_packaging = true;
    info.project_name = Some("Flask".to_string());
    assert!(!is_python_hijack(&info, "flask"));
}

/// If the user asked for the Python name itself (after
/// normalization), there is no mismatch and nothing to flip —
/// building `llama-cpp-scripts` on purpose must stay on the Python
/// path.
#[test]
pub(super) fn python_hijack_does_not_fire_when_the_user_wanted_the_python_name() {
    let mut info = bare_info();
    info.python_packaging = true;
    info.cmake_at_root = true;
    info.project_name = Some("llama-cpp-scripts".to_string());
    assert!(!is_python_hijack(&info, "llama_cpp_scripts"));
    assert!(!is_python_hijack(&info, "Llama-CPP-Scripts"));
}

#[test]
pub(super) fn release_endpoints_match_the_parsed_host_not_a_substring() {
    // A lookalike host is a Gitea-style forge, never the GitHub API.
    let e = release_endpoints("mygithub.com", "o/r");
    assert!(e[0].starts_with("https://mygithub.com/api/v1/"), "{e:?}");
    assert!(!e.iter().any(|u| u.contains("api.github.com")), "{e:?}");
    // Likewise a host that merely contains "gitlab" without the label.
    let e = release_endpoints("notgitlab.com", "o/r");
    assert!(e[0].starts_with("https://notgitlab.com/api/v1/"), "{e:?}");
    assert!(!e.iter().any(|u| u.contains("/api/v4/")), "{e:?}");
    let e = release_endpoints("evil-gitlab.example", "o/r");
    assert!(
        e[0].starts_with("https://evil-gitlab.example/api/v1/"),
        "{e:?}"
    );
    // Self-hosted GitLab names: a label that is or starts with "gitlab".
    for host in [
        "gitlab.example.org",
        "gitlab-internal.corp.example",
        "git.gitlab.corp.example",
    ] {
        let e = release_endpoints(host, "g/p");
        assert_eq!(
            e[0],
            format!("https://{host}/api/v4/projects/g%2Fp/releases?per_page=1"),
        );
        assert_eq!(
            e[1],
            format!("https://{host}/api/v4/projects/g%2Fp/repository/tags?per_page=1"),
        );
    }
    // The tag listing takes the same routes.
    assert!(tag_list_url("mygithub.com", "o/r").starts_with("https://mygithub.com/api/v1/"),);
    assert!(
        tag_list_url("git.gitlab.corp.example", "o/r")
            .starts_with("https://git.gitlab.corp.example/api/v4/"),
    );
    // The real hosts keep their APIs.
    assert_eq!(
        release_endpoints("github.com", "o/r")[0],
        "https://api.github.com/repos/o/r/releases/latest"
    );
    assert_eq!(
        release_endpoints("www.github.com", "o/r")[1],
        "https://api.github.com/repos/o/r/tags?per_page=1"
    );
    assert_eq!(
        release_endpoints("gitlab.gnome.org", "GNOME/glib")[0],
        "https://gitlab.gnome.org/api/v4/projects/GNOME%2Fglib/releases?per_page=1"
    );
    assert_eq!(
        release_endpoints("gitlab.com", "g/sub/proj")[1],
        "https://gitlab.com/api/v4/projects/g%2Fsub%2Fproj/repository/tags?per_page=1"
    );
    assert_eq!(
        release_endpoints("codeberg.org", "o/r")[0],
        "https://codeberg.org/api/v1/repos/o/r/releases/latest"
    );
}

#[test]
pub(super) fn source_kind_tells_forges_indexes_and_archives_apart() {
    use SourceKind::*;
    assert_eq!(source_kind("https://github.com/oatpp/oatpp"), Forge);
    assert_eq!(source_kind("https://gitlab.gnome.org/GNOME/glib"), Forge);
    assert_eq!(source_kind("https://git.example.com/owner/repo"), Forge);
    assert_eq!(
        source_kind("https://download.savannah.gnu.org/releases/lzip/"),
        ReleaseIndex
    );
    assert_eq!(source_kind("https://ftp.gnu.org/gnu/hello"), ReleaseIndex);
    assert_eq!(
        source_kind("https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz"),
        DirectArchive
    );
    assert_eq!(
        source_kind("https://example.org/dl/foo-1.0.tar.xz?x=1"),
        DirectArchive
    );
}

#[test]
pub(super) fn fixed_release_assets_are_direct_archives_but_tag_urls_stay_forge() {
    use SourceKind::*;
    assert_eq!(
        source_kind("https://github.com/o/r/releases/download/v1/foo-1.tar.gz"),
        DirectArchive
    );
    assert_eq!(
        source_kind("https://github.com/o/r/archive/refs/tags/v1.0.tar.gz"),
        DirectArchive
    );
    assert_eq!(source_kind("https://github.com/o/r/releases/tag/v1"), Forge);
    assert_eq!(source_kind("https://github.com/o/r"), Forge);
}

#[test]
pub(super) fn versions_compare_ignoring_v_prefix_and_trailing_zero_components() {
    assert!(same_version("1.0", "1.0.0"));
    assert!(same_version("v1.0", "1.0"));
    assert!(same_version("1.2.3", "1.2.3"));
    assert!(!same_version("1.0", "1.0.1"));
    assert!(!same_version("1.10", "1.1"));
    assert_eq!(
        archive_urls_non_forge(
            "https://x.org/foo-1.0.tar.gz",
            SourceKind::DirectArchive,
            "1.0.0"
        ),
        vec!["https://x.org/foo-1.0.tar.gz".to_string()]
    );
}

#[test]
pub(super) fn double_separator_names_keep_matching_their_stored_entry() {
    let m: SourceMap = toml::from_str("[source]\nfoo__bar = \"https://x.org/a\"\n").unwrap();
    assert_eq!(m.get("foo--bar"), Some("https://x.org/a"));
    assert_eq!(m.get("foo__bar"), Some("https://x.org/a"));
    let mut m = m;
    m.insert("foo--bar", "https://x.org/b");
    assert_eq!(m.urls.len(), 1);
    // A name with no stored entry still uses the collapsed key.
    m.insert("baz..qux", "https://x.org/c");
    assert_eq!(m.get("Baz-Qux"), Some("https://x.org/c"));
}

#[test]
pub(super) fn archive_name_and_version_are_split_at_the_last_dash_before_a_digit() {
    let nv = |f| archive_name_version(f);
    assert_eq!(
        nv("hello-2.12.tar.gz"),
        Some(("hello".into(), "2.12".into()))
    );
    assert_eq!(
        nv("foo-bar-2.0.1.tgz"),
        Some(("foo-bar".into(), "2.0.1".into()))
    );
    assert_eq!(
        nv("lib_x-1.0rc1.tar.gz"),
        Some(("lib_x".into(), "1.0rc1".into()))
    );
    assert_eq!(nv("README"), None);
    assert_eq!(nv("noversion.tar.gz"), None);
    assert_eq!(nv("-1.0.tar.gz"), None);
}

/// The positive half: the right host over https does get its token, and a
/// self-hosted GitLab only when `GITLAB_HOST` names it.
#[test]
pub(super) fn forge_tokens_are_attached_to_the_matching_host_only() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved: Vec<_> = [
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "GITLAB_TOKEN",
        "CI_JOB_TOKEN",
        "GITLAB_HOST",
    ]
    .iter()
    .map(|k| (*k, std::env::var(k).ok()))
    .collect();
    unsafe {
        for (k, _) in &saved {
            std::env::remove_var(k);
        }
        std::env::set_var("GITHUB_TOKEN", "gh-secret");
        std::env::set_var("GITLAB_TOKEN", "gl-secret");
    }
    let gh = forge_auth_header("https://api.github.com/repos/o/r/tags");
    let gl = forge_auth_header("https://gitlab.com/api/v4/projects?search=x");
    let selfhosted_before = forge_auth_header("https://git.example.org/api/v4/projects");
    unsafe { std::env::set_var("GITLAB_HOST", "git.example.org") };
    let selfhosted_after = forge_auth_header("https://git.example.org/api/v4/projects");
    let other = forge_auth_header("https://evil.example.net/api/v4/projects");
    unsafe {
        for (k, v) in &saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
    assert_eq!(gh, Some(("Authorization", "Bearer gh-secret".to_string())));
    assert_eq!(gl, Some(("PRIVATE-TOKEN", "gl-secret".to_string())));
    assert_eq!(selfhosted_before, None);
    assert_eq!(
        selfhosted_after,
        Some(("PRIVATE-TOKEN", "gl-secret".to_string()))
    );
    assert_eq!(other, None);
}

#[test]
pub(super) fn gitlab_tokens_use_the_header_gitlab_expects() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    };
    assert_eq!(
        gitlab_auth(env(&[("CI_JOB_TOKEN", "job")])),
        Some(("JOB-TOKEN", "job".to_string()))
    );
    assert_eq!(
        gitlab_auth(env(&[("GITLAB_TOKEN", "pat"), ("CI_JOB_TOKEN", "job")])),
        Some(("PRIVATE-TOKEN", "pat".to_string()))
    );
    assert_eq!(gitlab_auth(env(&[])), None);
}

#[test]
pub(super) fn forge_tokens_go_to_exact_hosts_over_https_only() {
    // SAFETY of test env: variables are process-global; use unlikely values
    // and only assert the negative cases plus host routing.
    assert_eq!(forge_auth_header("https://evil.com/api.github.com/x"), None);
    assert_eq!(
        forge_auth_header("https://gitlab-mirror.evil.com/a/b"),
        None
    );
    assert_eq!(forge_auth_header("http://api.github.com/x"), None);
    assert_eq!(forge_auth_header("https://notgitlab.com/api/v4/x"), None);
}
