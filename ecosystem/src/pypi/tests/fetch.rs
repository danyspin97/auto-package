use super::*;

/// A wheel-only release — NVIDIA's CUDA toolkit packages are the real
/// case — has `info.version` but no sdist. `parse_release` refuses it,
/// which is correct for the full sdist consumers; `parse_version_only`
/// must still succeed, because the version is the only thing an update
/// sweep is asking for.
const WHEEL_ONLY: &str = r#"{
  "info": {
    "name": "nvidia-nccl-cu13",
    "version": "2.29.7",
    "classifiers": [],
    "project_urls": {}
  },
  "urls": [
    {
      "digests": {"sha256": "deadbeef"},
      "filename": "nvidia_nccl_cu13-2.29.7-py3-none-manylinux2014_x86_64.whl",
      "packagetype": "bdist_wheel",
      "size": 123,
      "url": "https://files.pythonhosted.org/packages/x/nvidia_nccl_cu13-2.29.7-py3-none-manylinux2014_x86_64.whl",
      "yanked": false
    }
  ]
}"#;

#[test]
fn safe_filename_rejects_anything_but_a_bare_name() {
    assert_eq!(safe_filename("a-1.0.tar.gz"), Some("a-1.0.tar.gz"));
    for bad in ["../x", "/etc/passwd", "a/b", "..", ".", ""] {
        assert_eq!(safe_filename(bad), None, "{bad:?}");
    }
}

#[test]
fn fetch_refuses_a_traversing_filename() {
    let dir = tempfile::tempdir().unwrap();
    let s = Sdist {
        filename: "../evil".into(),
        url: "http://127.0.0.1:1/x".into(),
        sha256: String::new(),
        size: 0,
    };
    assert!(matches!(s.fetch(dir.path()), Err(FetchError::Io { .. })));
}

#[test]
fn parse_version_only_works_on_wheel_only_release() {
    // The whole point of this function: wheel-only releases have no sdist,
    // but still have a valid info.version. The full parser would error
    // here (NoSdist) and the update sweep would lose the ability to flag
    // a bump for every NVIDIA CUDA package.
    assert_eq!(parse_version_only(WHEEL_ONLY).unwrap(), "2.29.7");
    assert!(matches!(
        parse_release(WHEEL_ONLY),
        Err(Error::NoSdist { .. })
    ));
}

#[test]
fn source_url_from_json_follows_the_source_project_url_key() {
    let r = source_url_from_json(MARKUPSAFE);
    assert_eq!(r.as_deref(), Some("https://github.com/pallets/markupsafe"));
}

#[test]
fn source_url_from_json_preserves_nested_gitlab_group() {
    // The real bug: a project_urls entry like
    // `https://gitlab.com/group/subgroup/project` used to be truncated
    // to `group/subgroup`, which is a group URL, not a project URL.
    let json = r#"{
      "info": {
        "name": "x", "version": "1",
        "project_urls": {
          "Source": "https://gitlab.com/group/subgroup/project"
        }
      },
      "urls": []
    }"#;
    assert_eq!(
        source_url_from_json(json).as_deref(),
        Some("https://gitlab.com/group/subgroup/project"),
    );
}

#[test]
fn source_url_from_json_strips_github_subpaths() {
    let json = r#"{
      "info": {
        "name": "x", "version": "1",
        "project_urls": {
          "Source": "https://github.com/pallets/markupsafe/tree/main"
        }
      },
      "urls": []
    }"#;
    assert_eq!(
        source_url_from_json(json).as_deref(),
        Some("https://github.com/pallets/markupsafe"),
    );
}

#[test]
fn parse_version_only_ignores_the_urls_array_entirely() {
    // Even if the urls array is completely absent (malformed response),
    // info.version is still extractable. Belt-and-braces: the whole point
    // is that this path never touches urls.
    let json = r#"{"info":{"name":"foo","version":"1.2.3"}}"#;
    assert_eq!(parse_version_only(json).unwrap(), "1.2.3");
}

/// Live PyPI check of the wheel lookup `--nvidia-redist` and the wheel switch
/// use: public GETs only. Run with `cargo test -- --ignored`.
#[test]
#[ignore = "network: GETs pypi.org"]
fn live_pypi_wheel_lookup() {
    // A wheel-only NVIDIA release: no sdist, one manylinux wheel per arch.
    let json = get_json("https://pypi.org/pypi/nvidia-nvjitlink/json").unwrap();
    let wheel = pick_wheel_from_json(&json, "x86_64", None).unwrap();
    assert!(
        wheel.filename.ends_with(".whl") && wheel.filename.contains("x86_64"),
        "{}",
        wheel.filename
    );
    assert_eq!(wheel.sha256.len(), 64);
    assert!(
        wheel.url.starts_with("https://files.pythonhosted.org/"),
        "{}",
        wheel.url
    );
    assert!(pypi_name_and_version_from_wheel_filename(&wheel.filename).is_some());
    let aarch = pick_wheel_from_json(&json, "aarch64", None).unwrap();
    assert!(aarch.filename.contains("aarch64"), "{}", aarch.filename);
    // A CPython-locked family: one wheel per flavor.
    let tri = get_json("https://pypi.org/pypi/nvidia-cutlass-dsl-libs-cu13/json").unwrap();
    let abis = vec!["cp313".to_string(), "cp314".to_string()];
    let per = pick_wheels_per_flavor(&tri, "x86_64", &abis).unwrap();
    assert!(per[0].1.filename.contains("cp313") && per[1].1.filename.contains("cp314"));
    // The release endpoints.
    let release = fetch_latest_release("flask").unwrap();
    assert!(!release.version.is_empty() && !release.sdist.url.is_empty());
    assert!(fetch_latest_version("nvidia-nvjitlink").is_ok());
    assert!(
        fetch_latest_release("this-package-does-not-exist-ap-r3a")
            .unwrap_err()
            .is_not_found()
    );
}
