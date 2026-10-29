use super::*;

/// Live resolution of an sdist's source repository from the Project-URL
/// entries PyPI serves for real projects: public GETs only. Run with
/// `cargo test -- --ignored`.
#[test]
#[ignore = "network: GETs pypi.org"]
fn live_source_repo_from_the_metadata_pypi_serves() {
    for (project, want) in [
        ("flask", "https://github.com/pallets/flask"),
        ("requests", "https://github.com/psf/requests"),
        ("numpy", "https://github.com/numpy/numpy"),
    ] {
        let url = format!("https://pypi.org/pypi/{project}/json");
        let body = crate::net::call_with_retries(|| {
            crate::net::agent(ureq::Agent::config_builder()).get(&url)
        })
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let info = &json["info"];
        let mut pkg_info = String::from("Metadata-Version: 2.1\n");
        if let Some(urls) = info["project_urls"].as_object() {
            for (k, v) in urls {
                pkg_info.push_str(&format!("Project-URL: {k}, {}\n", v.as_str().unwrap_or("")));
            }
        }
        if let Some(h) = info["home_page"].as_str().filter(|h| !h.is_empty()) {
            pkg_info.push_str(&format!("Home-page: {h}\n"));
        }
        assert_eq!(
            source_repo_from_metadata(&pkg_info, "").as_deref(),
            Some(want),
            "{project}: {pkg_info}"
        );
    }
}

/// Every requirement a spec can carry verbatim is accepted; anything that
/// could open a spec line or expand a macro is refused.
#[test]
fn build_requirement_gate_accepts_pep508_and_refuses_injection() {
    for good in [
        "setuptools",
        "setuptools>=42",
        "setuptools >= 42, < 80",
        "flit_core>=3.2.0,<4",
        "numpy==1.*",
        "cython~=3.0",
        "pkg[extra1,extra2]>=1.0",
        "Foo.Bar-baz_qux",
        "pkg!=1.0",
        "setuptools (>=42)",
        "foo (>=1.0, <2)",
        "pkg[extra] (>=1.0)",
        "foo(>=1)",
    ] {
        assert!(is_safe_build_requirement(good), "{good:?}");
    }
    for bad in [
        "",
        " ",
        "setuptools\n%pre\necho pwned",
        "setuptools\n>=1",
        "setuptools\r\nBuildRequires: x",
        "a%{x}",
        "a{b}",
        "a$(x)",
        "a`x`",
        "a;b",
        "a|b",
        "a&b",
        "a'b",
        "a\"b",
        "a\\b",
        "a#b",
        "a\tb",
        "pkg ; python_version < '3'",
        "pkg (>=1.0",
        "pkg (>=1.0))",
        "pkg ()",
        "pkg (>=1; $(x))",
        "pkg (>=1.0) (<2)",
        "pkg (>=1.0 $(x))",
        "pkg @ https://example.org/x.tar.gz",
        "pkg[extra",
        "pkg[]",
        "pkg>=",
        "pkg>=1.0 $(x)",
        "-r requirements.txt",
        "pkg==1.0 %{version}",
        "é",
    ] {
        assert!(!is_safe_build_requirement(bad), "{bad:?}");
    }
}

/// A hostile `[build-system] requires` loses only the bad entries.
#[test]
fn build_system_requires_drops_injected_entries_at_the_source() {
    let src = "[build-system]\nrequires = [\"setuptools\\n%pre\\necho x\", \"wheel\", \"a; $(x)\", \"cython>=3\"]\nbuild-backend = \"setuptools.build_meta\"\n";
    let bs = build_system_from_toml(src);
    assert_eq!(bs.requires, ["wheel", "cython>=3"]);
}

#[test]
fn shell_safe_member_and_manifest_dir_checks() {
    for good in [
        "pkg/__init__.py",
        "pkg-1.0.dist-info/METADATA",
        "a/b/c.so.1",
    ] {
        assert!(is_shell_safe_member(good), "{good}");
    }
    for bad in [
        "", "a b", "a\nb", "a%b", "a{b}", "a$(x)", "a`x`", "a;b", "a&b", "a|b", "a(b)", "a<b",
        "a>b", "a!b", "a#b", "a/../b", "a//b", "./a", "a\\b", "a*b", "a?b",
    ] {
        assert!(!is_shell_safe_member(bad), "{bad:?}");
    }
    assert!(is_safe_manifest_dir(""));
    assert!(is_safe_manifest_dir("python/cuda_core"));
    for bad in [
        "a/$(x)", "a/b c", "a/../b", "a/`x`", "a/%{x}", "/abs", "a/;b",
    ] {
        assert!(!is_safe_manifest_dir(bad), "{bad:?}");
    }
    assert!(is_python_identifier("cuda_core2"));
    for bad in ["", "2pkg", "a-b", "a.b", "a b", "a;b", "a$(x)", "é"] {
        assert!(!is_python_identifier(bad), "{bad:?}");
    }
}

/// A manifest under an unsafe directory component is not recorded, so the
/// monorepo siblings that remain are the only ones a spec ever names.
#[test]
fn a_manifest_under_an_unsafe_directory_is_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("mono-1.0.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "mono-1.0/pyproject.toml",
                b"[project]\nname = \"root\"\n".as_slice(),
            ),
            (
                "mono-1.0/pkgs/a/pyproject.toml",
                b"[project]\nname = \"a\"\n".as_slice(),
            ),
            (
                "mono-1.0/pkgs/b/pyproject.toml",
                b"[project]\nname = \"b\"\n".as_slice(),
            ),
            (
                "mono-1.0/pkgs/$(touch x)/pyproject.toml",
                b"[project]\nname = \"c\"\n".as_slice(),
            ),
            (
                "mono-1.0/pkgs/d e/pyproject.toml",
                b"[project]\nname = \"d\"\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    for sub in &info.subprojects {
        assert!(is_safe_manifest_dir(&sub.dir), "{:?}", sub.dir);
        assert!(
            !sub.dir.contains('$') && !sub.dir.contains(' '),
            "{:?}",
            sub.dir
        );
    }
}

/// An importable name or a shared-library path that is not shell-safe is
/// not offered: the layout reports no name the spec would write.
#[test]
fn wheel_names_that_are_not_shell_safe_are_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("evil.whl");
    write_wheel_with(
        &w,
        &[
            ("evil;touch x/__init__.py", b""),
            ("a$(touch y)/__init__.py", b""),
            ("a$(touch y)/lib/libz.so.1", b""),
            ("bad`x`.dist-info/METADATA", b"Name: bad\n"),
        ],
    );
    let layout = wheel_layout(&w).unwrap();
    assert_eq!(layout.importable, None);
    assert_eq!(layout.first_shared_lib, None);
    assert_eq!(layout.dist_info, None);

    let ok = dir.path().join("ok.whl");
    write_wheel_with(
        &ok,
        &[("pkg/__init__.py", b""), ("pkg/_native.abi3.so", b"")],
    );
    let layout = wheel_layout(&ok).unwrap();
    assert_eq!(layout.importable.as_deref(), Some("pkg"));
    assert_eq!(
        layout.first_shared_lib.as_deref(),
        Some("pkg/_native.abi3.so")
    );
}
