use super::*;

mod formats;
mod limits;
mod links;
mod project;
mod requirements;
mod safety;
mod wheels;

use formats::*;
use links::*;
use project::*;
use wheels::*;

/// Every entry lands in `files`, including the manifests — callers decide
/// what a tarball is by looking for `pyproject.toml` and `Cargo.toml`
/// there, and reading one must not remove it from the listing.
#[test]
fn the_manifests_appear_in_the_file_listing() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/Cargo.toml",
                b"[package]\nname = \"thing\"\n".as_slice(),
            ),
            (
                "thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing-py\"\n".as_slice(),
            ),
            ("thing-1.0/src/main.rs", b"fn main() {}".as_slice()),
        ],
    );

    let info = inspect(&tarball).unwrap();
    assert!(
        info.files.iter().any(|f| f == "thing-1.0/Cargo.toml"),
        "{:?}",
        info.files
    );
    assert!(
        info.files.iter().any(|f| f == "thing-1.0/pyproject.toml"),
        "{:?}",
        info.files
    );
    // Both manifests present: the Python name wins (maturin projects).
    assert_eq!(info.project_name.as_deref(), Some("thing-py"));
}

#[test]
fn a_declared_name_that_is_not_a_plain_project_name_is_dropped() {
    for ok in ["Flask", "zope.interface", "a_b-c", "libsigc++"] {
        assert!(is_plain_project_name(ok), "{ok}");
    }
    for bad in ["", "../x", "a/b", "a b", "-x", "a;rm", "a<b", "a%{x}", "é"] {
        assert!(!is_plain_project_name(bad), "{bad}");
    }
}

#[test]
fn pyproject_scripts_are_read_and_unsafe_names_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[(
            "thing-1/pyproject.toml",
            b"[project]\nname = \"thing\"\n[project.scripts]\nthing = \"thing:main\"\n\
                  \"../x\" = \"a:b\"\n[project.gui-scripts]\nthing-gui = \"thing:gui\"\n"
                .as_slice(),
        )],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.console_scripts, vec!["thing", "thing-gui"]);
}

#[test]
fn a_poetry_only_pyproject_still_yields_its_declared_name() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "llama.cpp-b1/pyproject.toml",
                b"[tool.poetry]\nname = \"llama-cpp-scripts\"\n".as_slice(),
            ),
            (
                "llama.cpp-b1/CMakeLists.txt",
                b"project(llama)\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert!(info.python_packaging);
    assert!(info.cmake_at_root);
    assert!(!info.cmake_manifest);
    assert_eq!(info.project_name.as_deref(), Some("llama-cpp-scripts"));
}

#[test]
fn a_top_level_cmakelists_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "libthing-1.0/CMakeLists.txt",
                b"project(thing)\n".as_slice(),
            ),
            ("libthing-1.0/src/thing.c", b"int main(){}".as_slice()),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert!(info.cmake_manifest);
    assert!(!info.autotools_manifest);
}

#[test]
fn top_level_tests_dir_is_found_but_a_nested_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing\"\n".as_slice(),
            ),
            ("thing-1.0/tests/test_thing.py", b"".as_slice()),
            // A nested test-looking file elsewhere must not win over the
            // real top-level tests/ dir, nor substitute for it when the
            // real one is absent.
            ("thing-1.0/thing/scripts/test_fixture.py", b"".as_slice()),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.top_level_tests_dir.as_deref(), Some("tests"));

    let tarball2 = dir.path().join("src2.tar.gz");
    write_tar(
        &tarball2,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing\"\n".as_slice(),
            ),
            ("thing-1.0/thing/scripts/test_fixture.py", b"".as_slice()),
        ],
    );
    let info2 = inspect(&tarball2).unwrap();
    assert_eq!(info2.top_level_tests_dir, None);
}

#[test]
fn a_nested_cmakelists_is_not_the_project() {
    // A vendored dependency's CMakeLists.txt two levels down must not
    // make an otherwise plain sdist look like a CMake project.
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
                "thing-1.0/vendor/lib/CMakeLists.txt",
                b"project(vendored)\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert!(!info.cmake_manifest);
}

#[test]
fn a_top_level_configure_ac_is_detected_as_autotools() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "libthing-1.0/configure.ac",
                b"AC_INIT([thing], [1.0])\n".as_slice(),
            ),
            (
                "libthing-1.0/Makefile.am",
                b"bin_PROGRAMS = thing\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert!(info.autotools_manifest);
    assert!(!info.cmake_manifest);
}

#[test]
fn cmake_alongside_python_packaging_is_not_a_plain_c_project() {
    // scikit-build-core Python extensions carry both — Python wins, per
    // convention 3, not the new plain-C/C++ path.
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing\"\n".as_slice(),
            ),
            ("thing-1.0/CMakeLists.txt", b"project(thing)\n".as_slice()),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert!(!info.cmake_manifest);
    assert!(info.python_packaging);
}

#[test]
fn a_crate_reports_its_cargo_name() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[(
            "ripgrep-14.1.1/Cargo.toml",
            b"[package]\nname = \"ripgrep\"\n".as_slice(),
        )],
    );
    assert_eq!(
        inspect(&tarball).unwrap().project_name.as_deref(),
        Some("ripgrep")
    );
}

/// NVIDIA's archives are `.tar.xz`: the member list comes from the stream,
/// directories keep their trailing slash, and nothing is unpacked.
#[test]
fn list_archive_members_reads_a_tar_xz_without_extracting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("cuda_cudart-linux-x86_64-13.4.92-archive.tar.xz");
    let mut raw = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut raw);
        let mut d = tar::Header::new_gnu();
        d.set_entry_type(tar::EntryType::Directory);
        d.set_size(0);
        d.set_mode(0o755);
        d.set_cksum();
        ar.append_data(&mut d, "top/lib", std::io::empty()).unwrap();
        for name in ["top/LICENSE", "top/lib/libx.so.1"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(2);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, name, &b"hi"[..]).unwrap();
        }
        ar.finish().unwrap();
    }
    let mut w = lzma_rust2::XzWriter::new(
        std::fs::File::create(&path).unwrap(),
        lzma_rust2::XzOptions::default(),
    )
    .unwrap();
    std::io::Write::write_all(&mut w, &raw).unwrap();
    w.finish().unwrap();

    assert_eq!(
        list_archive_members(&path).unwrap(),
        ["top/lib/", "top/LICENSE", "top/lib/libx.so.1"]
    );
    // Only the archive is in the directory.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn list_archive_members_refuses_a_zip() {
    assert!(list_archive_members(Path::new("x.whl")).is_err());
}

/// A monorepo snapshot holds several projects side by side; which one is
/// "the" project is only answerable by knowing which was asked for.
#[test]
fn a_monorepo_yields_the_subproject_that_was_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "cuda-python-13.0.2/cuda_bindings/pyproject.toml",
                b"[project]\nname = \"cuda-bindings\"\n".as_slice(),
            ),
            (
                "cuda-python-13.0.2/cuda_core/pyproject.toml",
                b"[project]\nname = \"cuda-core\"\n".as_slice(),
            ),
            (
                "cuda-python-13.0.2/cuda_python/pyproject.toml",
                b"[project]\nname = \"cuda-python\"\n".as_slice(),
            ),
        ],
    );

    let info = inspect_preferring(&tarball, Some("cuda-python")).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("cuda-python"));
    assert_eq!(info.project_dir.as_deref(), Some("cuda_python"));
    assert!(info.python_packaging);

    // The siblings are not dropped — a monorepo usually wants all of them.
    assert_eq!(
        info.subprojects,
        vec![
            Subproject {
                dir: "cuda_bindings".into(),
                name: Some("cuda-bindings".into())
            },
            Subproject {
                dir: "cuda_core".into(),
                name: Some("cuda-core".into())
            },
            Subproject {
                dir: "cuda_python".into(),
                name: Some("cuda-python".into())
            },
        ]
    );

    // Asking for a sibling gets that sibling.
    let info = inspect_preferring(&tarball, Some("cuda_core")).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("cuda-core"));
    assert_eq!(info.project_dir.as_deref(), Some("cuda_core"));
}

/// A meson project has no manifest auto-package classifies: its build file is
/// `meson.build` at the top. The top-level directory is still read from the
/// archive, which is what a carried-forward `%autosetup -n` needs.
#[test]
fn a_meson_tarball_gives_its_top_directory_and_no_build_system() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("cagebreak-3.2.1.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "cagebreak-3.2.1/meson.build",
                b"project('cagebreak', 'c')\n".as_slice(),
            ),
            (
                "cagebreak-3.2.1/src/main.c",
                b"int main(void) { return 0; }\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.top_level_dir.as_deref(), Some("cagebreak-3.2.1"));
    assert!(
        info.files
            .iter()
            .any(|f| f == "cagebreak-3.2.1/meson.build")
    );
    assert!(!info.python_packaging);
    assert!(!info.cargo_manifest);
    assert!(!info.cmake_at_root);
    assert!(!info.autotools_at_root);
    // Nothing declares a name for it: the requested name is kept.
    assert_eq!(info.project_name, None);
}

/// The top-level directory is the first directory of the archive, whether it
/// is named after the repository or after the repository and the version, and
/// a `pax_global_header` entry in front of it does not count.
#[test]
fn a_forge_archive_gives_its_top_directory_after_a_pax_header() {
    let dir = tempfile::tempdir().unwrap();
    for top in ["cagebreak", "cagebreak-3.2.1"] {
        let tarball = dir.path().join(format!("{top}.tar.gz"));
        let meson = format!("{top}/meson.build");
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
            add("pax_global_header", b"");
            add(&meson, b"project('cagebreak', 'c')\n");
            ar.into_inner().unwrap().finish().unwrap();
        }
        let info = inspect(&tarball).unwrap();
        assert_eq!(info.top_level_dir.as_deref(), Some(top), "{top}");
        assert!(info.files.iter().any(|f| *f == meson), "{top}");
    }
}
