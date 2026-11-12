use super::*;

/// The chosen pyproject decides the project directory. Here `cuda_bindings`
/// has only a legacy setup.py and `cuda_core` the pyproject, and neither
/// directory matches the package name: the build must `cd` into `cuda_core`.
#[test]
fn project_dir_follows_the_chosen_pyproject_not_the_first_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            ("thing-1.0/cuda_bindings/setup.py", b"x".as_slice()),
            (
                "thing-1.0/cuda_core/pyproject.toml",
                b"[build-system]\nrequires = [\"setuptools\"]\n[project]\nname = \"other-pkg\"\n"
                    .as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("other-pkg"));
    assert_eq!(info.project_dir.as_deref(), Some("cuda_core"));
}

/// A pyproject.toml that is only tool config (no `[build-system]` or
/// `[project]`) does not name the project directory: the root setup.py does.
#[test]
fn tool_only_pyproject_does_not_name_the_project_directory() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/setup.py",
                b"from setuptools import setup\n".as_slice(),
            ),
            (
                "thing-1.0/tests/pyproject.toml",
                b"[tool.pytest.ini_options]\naddopts = \"-q\"\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.project_dir, None);
    assert!(info.python_packaging);
}

#[test]
fn a_manifest_at_the_root_beats_a_matching_subdirectory() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing\"\n".as_slice(),
            ),
            (
                "thing-1.0/examples/demo/pyproject.toml",
                b"[project]\nname = \"demo\"\n".as_slice(),
            ),
        ],
    );
    let info = inspect_preferring(&tarball, Some("demo")).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("thing"));
    assert_eq!(info.project_dir, None);
    // One project at the root is not a monorepo, whatever sits below it.
    assert!(info.subprojects.is_empty());
}

/// Without a packaging pyproject.toml, `examples/setup.py` must not win the
/// alphabetical tie against the real binding in `python/setup.py`.
#[test]
fn a_binding_directory_beats_examples_in_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            ("pkg-1.0/examples/setup.py", b"x".as_slice()),
            ("pkg-1.0/python/setup.py", b"x".as_slice()),
        ],
    );
    let info = inspect_preferring(&tarball, Some("pkg")).unwrap();
    assert_eq!(info.project_dir.as_deref(), Some("python"));
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.project_dir.as_deref(), Some("python"));
}

/// Several candidates and none of them product-like: no guess is made.
#[test]
fn the_fallback_refuses_to_guess_between_several_directories() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            ("pkg-1.0/core/setup.py", b"x".as_slice()),
            ("pkg-1.0/util/setup.py", b"x".as_slice()),
            ("pkg-1.0/examples/setup.py", b"x".as_slice()),
        ],
    );
    let info = inspect_preferring(&tarball, Some("pkg")).unwrap();
    assert_eq!(info.project_dir, None);
    assert!(info.python_packaging);
}

#[test]
fn fallback_prefers_the_named_directory_then_bindings_then_the_only_product() {
    let s = |v: &[&str]| v.iter().map(|d| d.to_string()).collect::<Vec<_>>();
    // A directory named after the project wins over a binding directory.
    assert_eq!(
        choose_fallback_dir(s(&["python", "my-pkg"]), Some("my_pkg")).as_deref(),
        Some("my-pkg")
    );
    // Binding names in their preference order: `python` before `src`.
    assert_eq!(
        choose_fallback_dir(s(&["src", "lib", "python"]), Some("pkg")).as_deref(),
        Some("python")
    );
    assert_eq!(
        choose_fallback_dir(s(&["src", "lib"]), None).as_deref(),
        Some("src")
    );
    // A lone candidate is taken, even a non-product one.
    assert_eq!(
        choose_fallback_dir(s(&["examples"]), Some("pkg")).as_deref(),
        Some("examples")
    );
    assert_eq!(
        choose_fallback_dir(s(&["core"]), None).as_deref(),
        Some("core")
    );
    // Non-product directories do not compete with the one product directory.
    assert_eq!(
        choose_fallback_dir(s(&["examples", "docs", "core"]), Some("pkg")).as_deref(),
        Some("core")
    );
    // Duplicates (a directory with both setup.py and Cargo.toml) are one candidate.
    assert_eq!(
        choose_fallback_dir(s(&["core", "core"]), None).as_deref(),
        Some("core")
    );
    // A binding directory below a non-product one is still a binding.
    assert_eq!(
        choose_fallback_dir(s(&["bindings/python", "examples/demo"]), None).as_deref(),
        Some("bindings/python")
    );
}

#[test]
fn fallback_refuses_when_only_non_product_or_several_product_directories_remain() {
    let s = |v: &[&str]| v.iter().map(|d| d.to_string()).collect::<Vec<_>>();
    assert_eq!(choose_fallback_dir(s(&["examples", "docs"]), None), None);
    assert_eq!(choose_fallback_dir(s(&["core", "util"]), Some("pkg")), None);
    assert_eq!(choose_fallback_dir(Vec::new(), Some("pkg")), None);
}

/// A GitHub source tarball starts with a `pax_global_header` entry at the
/// archive root. Three real packages (cuda-python, torchcodec, triton)
/// shipped `%autosetup -n pax_global_header` because of it.
#[test]
fn pax_global_header_is_not_the_top_level_dir() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    {
        let file = std::fs::File::create(&tarball).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);

        let mut add = |name: &str, body: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, name, body).unwrap();
        };
        add("pax_global_header", b"52 comment=deadbeef\n");
        add("triton-3.5.0/setup.py", b"from setuptools import setup\n");

        ar.into_inner().unwrap().finish().unwrap();
    }

    let info = inspect(&tarball).unwrap();
    assert_eq!(info.top_level_dir.as_deref(), Some("triton-3.5.0"));
}

pub(super) fn build_system_from_toml(src: &str) -> BuildSystem {
    let doc: PyprojectToml = toml::from_str(src).unwrap();
    doc.build_system
        .map(BuildSystem::from)
        .unwrap_or_else(BuildSystem::setuptools_default)
}

#[test]
fn hatchling_backend() {
    let src = r#"
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
"#;
    let bs = build_system_from_toml(src);
    assert_eq!(bs.requires, ["hatchling"]);
    assert_eq!(bs.backend.as_deref(), Some("hatchling.build"));
}

#[test]
fn setuptools_with_extras() {
    let src = r#"
[build-system]
requires = ["setuptools>=42", "wheel"]
build-backend = "setuptools.build_meta"
"#;
    let bs = build_system_from_toml(src);
    assert_eq!(bs.requires, ["setuptools>=42", "wheel"]);
}

#[test]
fn no_build_system_section_defaults_to_setuptools() {
    let bs = build_system_from_toml("[project]\nname = \"foo\"\n");
    assert_eq!(bs.requires, ["setuptools"]);
    assert_eq!(bs.backend.as_deref(), Some("setuptools.build_meta"));
}

#[test]
fn no_pyproject_toml_defaults_to_setuptools() {
    // Simulated by calling the default constructor directly — a real
    // tarball without pyproject.toml reaches the same path in build_system().
    let bs = BuildSystem::setuptools_default();
    assert_eq!(bs.requires, ["setuptools"]);
}

#[test]
fn flit_core() {
    let src = r#"
[build-system]
requires = ["flit_core>=3.2.0,<4"]
build-backend = "flit_core.buildapi"
"#;
    let bs = build_system_from_toml(src);
    assert_eq!(bs.requires, ["flit_core>=3.2.0,<4"]);
    assert_eq!(bs.backend.as_deref(), Some("flit_core.buildapi"));
}

fn bins_of(manifest: &str, extra: &[&str]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    let mut entries: Vec<(String, Vec<u8>)> =
        vec![("thing-1.0/Cargo.toml".into(), manifest.as_bytes().to_vec())];
    for e in extra {
        entries.push((format!("thing-1.0/{e}"), b"fn main() {}".to_vec()));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    write_tar(&tarball, &refs);
    inspect(&tarball).unwrap().bin_names
}

#[test]
fn a_bin_table_names_the_executable_not_the_crate() {
    let m =
        "[package]\nname = \"jay-compositor\"\n[[bin]]\nname = \"jay\"\npath = \"src/main.rs\"\n";
    assert_eq!(bins_of(m, &["src/main.rs"]), ["jay"]);
}

#[test]
fn the_default_binary_is_named_after_the_package() {
    let m = "[package]\nname = \"thing\"\n";
    assert_eq!(bins_of(m, &["src/main.rs"]), ["thing"]);
    assert!(bins_of(m, &["src/lib.rs"]).is_empty());
}

#[test]
fn src_bin_files_and_directories_are_executables() {
    let m = "[package]\nname = \"thing\"\n";
    let got = bins_of(m, &["src/main.rs", "src/bin/a.rs", "src/bin/b/main.rs"]);
    assert_eq!(got, ["thing", "a", "b"]);
}

#[test]
fn autobins_false_drops_the_implicit_binaries() {
    let m = "[package]\nname = \"thing\"\nautobins = false\n[[bin]]\nname = \"t\"\npath = \"tools/t.rs\"\n";
    assert_eq!(bins_of(m, &["src/main.rs", "src/bin/a.rs"]), ["t"]);
}

#[test]
fn a_dot_slash_path_still_claims_src_main() {
    let m =
        "[package]\nname = \"jay-compositor\"\n[[bin]]\nname = \"jay\"\npath = \"./src/main.rs\"\n";
    assert_eq!(bins_of(m, &["src/main.rs"]), ["jay"]);
}

#[test]
fn a_bin_table_under_src_bin_is_not_counted_twice() {
    let m = "[package]\nname = \"thing\"\n[[bin]]\nname = \"x\"\npath = \"src/bin/foo.rs\"\n";
    assert_eq!(bins_of(m, &["src/bin/foo.rs"]), ["x"]);
}
