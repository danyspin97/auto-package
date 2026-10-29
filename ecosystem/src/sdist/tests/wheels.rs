use super::*;

#[test]
fn wheel_root_is_purelib_reads_the_wheel_metadata() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    for (value, expect) in [("true", true), ("false", false)] {
        let path = dir.path().join(format!("w-{value}.whl"));
        let file = std::fs::File::create(&path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        zw.start_file("pkg-1.0.dist-info/WHEEL", opts).unwrap();
        write!(
            zw,
            "Wheel-Version: 1.0\nRoot-Is-Purelib: {value}\nTag: py3-none-manylinux_2_17_x86_64\n"
        )
        .unwrap();
        zw.finish().unwrap();
        assert_eq!(wheel_root_is_purelib(&path), Some(expect));
    }
    assert_eq!(wheel_root_is_purelib(&dir.path().join("missing.whl")), None);
}

fn write_wheel(path: &Path, files: &[&str]) {
    use std::io::Write;
    let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    for f in files {
        zw.start_file(*f, opts).unwrap();
        zw.write_all(b"x").unwrap();
    }
    zw.finish().unwrap();
}

#[test]
fn wheel_layout_reads_the_files_dist_info_and_requires_dist() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("m.whl");
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&w).unwrap());
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    zw.start_file("pkg/__init__.py", opts).unwrap();
    zw.start_file("pkg-1.0.dist-info/METADATA", opts).unwrap();
    write!(
            zw,
            "Metadata-Version: 2.4\nName: pkg\nRequires-Dist: numpy>=1.20\nRequires-Dist: tomli; python_version < \"3.11\"\n\nRequires-Dist: not-a-header\n"
        )
        .unwrap();
    zw.finish().unwrap();
    let l = wheel_layout(&w).unwrap();
    assert_eq!(l.dist_info.as_deref(), Some("pkg-1.0.dist-info"));
    assert_eq!(l.files, ["pkg/__init__.py", "pkg-1.0.dist-info/METADATA"]);
    assert_eq!(
        l.requires_dist,
        ["numpy>=1.20", "tomli; python_version < \"3.11\""]
    );
}

#[test]
fn wheel_layout_reads_the_first_prose_paragraph_of_the_description() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("d.whl");
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&w).unwrap());
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    zw.start_file("pkg-1.0.dist-info/METADATA", opts).unwrap();
    write!(
            zw,
            "Metadata-Version: 2.4\nName: pkg\nSummary: s\n\n# pkg\n\n[![CI](x)](y)\n\nPkg is a fast thing that does\nuseful work, 100% of the time.\n\nSecond paragraph.\n"
        )
        .unwrap();
    zw.finish().unwrap();
    let l = wheel_layout(&w).unwrap();
    assert_eq!(
        l.description.as_deref(),
        Some("Pkg is a fast thing that does useful work, 100% of the time.")
    );
    assert_eq!(metadata_description("Name: x\n\n# only a heading\n"), None);
}

#[test]
fn metadata_description_reads_a_crlf_metadata_file() {
    let crlf = "Metadata-Version: 2.4\r\nName: pkg\r\nSummary: s\r\n\r\n# pkg\r\n\r\nPkg is a fast thing that does\r\nuseful work.\r\n\r\nSecond paragraph.\r\n";
    assert_eq!(
        metadata_description(crlf).as_deref(),
        Some("Pkg is a fast thing that does useful work.")
    );
    assert_eq!(
        metadata_description(&crlf.replace("\r\n", "\n")),
        metadata_description(crlf)
    );
}

#[test]
fn wheel_layout_finds_an_importable_package() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("a.whl");
    write_wheel(
        &w,
        &["pkg/__init__.py", "pkg/x.py", "pkg-1.0.dist-info/RECORD"],
    );
    let l = wheel_layout(&w).unwrap();
    assert_eq!(l.importable.as_deref(), Some("pkg"));
    assert_eq!(l.first_shared_lib, None);
}

/// NVIDIA's shape: a PEP 420 namespace with only shared libraries — nothing
/// to import, so the check must look at the file instead.
#[test]
fn wheel_layout_of_a_namespace_only_wheel_offers_a_file_to_check() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("n.whl");
    write_wheel(
        &w,
        &[
            "nvidia/cu13/lib/libnvJitLink.so.13",
            "nvidia/cu13/lib/deep/libother.so.1",
            "nvidia/cu13/include/nvJitLink.h",
            "nvidia_nvjitlink-1.0.dist-info/RECORD",
        ],
    );
    let l = wheel_layout(&w).unwrap();
    assert_eq!(l.importable, None);
    assert_eq!(
        l.first_shared_lib.as_deref(),
        Some("nvidia/cu13/lib/libnvJitLink.so.13")
    );
    assert_eq!(l.header_dirs, vec!["nvidia/cu13/include".to_string()]);
}

/// A header whose directory is not shell and RPM safe never reaches
/// `header_dirs`: it is written into `%install` by the spec generator.
#[test]
fn wheel_layout_drops_header_dirs_that_are_not_shell_safe() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("evil.whl");
    write_wheel(
        &w,
        &[
            "pkg/%{lua:os.execute(\"id\")}/a.h",
            "pkg/a;touch x/b.h",
            "pkg/$(x)/c.hpp",
            "pkg/a b/d.h",
            "pkg/ok/include/e.h",
            "pkg/q`x`/g.h",
            "pkg/__init__.py",
        ],
    );
    let l = wheel_layout(&w).unwrap();
    assert_eq!(l.header_dirs, vec!["pkg/ok/include".to_string()]);
    for bad in ["%{lua", ";", "$(", " ", "`"] {
        assert!(l.header_dirs.iter().all(|d| !d.contains(bad)), "{bad}");
    }
}

/// Multiple headers in the same directory collapse to one entry;
/// headers in distinct directories (and a `.hpp` alongside a `.h`) each
/// get their own, sorted. A `.dist-info` header (unlikely, but
/// hypothetically possible metadata) must not count.
#[test]
fn wheel_layout_dedupes_and_sorts_header_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("h.whl");
    write_wheel(
        &w,
        &[
            "nvidia/nccl/include/nccl.h",
            "nvidia/nccl/include/nccl_device.h",
            "nvidia/cublas/include/cublas_v2.hpp",
            "nvidia_nccl-1.0.dist-info/licenses/some.h",
        ],
    );
    let l = wheel_layout(&w).unwrap();
    assert_eq!(
        l.header_dirs,
        vec![
            "nvidia/cublas/include".to_string(),
            "nvidia/nccl/include".to_string(),
        ]
    );
}

/// The real nvidia-nccl-cu13 shape: headers nested many levels under one
/// top-level `include/` (`include/nccl_device/gin/gdaki/...`). Only the
/// shallowest directory should be kept — rpmlint's `addFilter` is an
/// unanchored substring match, so a filter on the parent already covers
/// every descendant, and one redundant filter per nested directory would
/// just be noise.
#[test]
fn wheel_layout_collapses_nested_header_dirs_to_the_shallowest_one() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("nested.whl");
    write_wheel(
        &w,
        &[
            "nvidia/nccl/include/nccl.h",
            "nvidia/nccl/include/nccl_device/comm.h",
            "nvidia/nccl/include/nccl_device/gin/gdaki/common.h",
        ],
    );
    let l = wheel_layout(&w).unwrap();
    assert_eq!(l.header_dirs, vec!["nvidia/nccl/include".to_string()]);
}

/// `a/b-x` sorts between `a/b` and its child `a/b/c`, so a check against
/// only the last kept directory would keep `a/b/c` as a redundant filter.
#[test]
fn wheel_layout_collapses_children_even_when_a_sibling_sorts_between() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("between.whl");
    write_wheel(&w, &["pkg/b/x.h", "pkg/b-x/y.h", "pkg/b/c/z.h"]);
    let l = wheel_layout(&w).unwrap();
    assert_eq!(
        l.header_dirs,
        vec!["pkg/b".to_string(), "pkg/b-x".to_string()]
    );
}

pub(super) fn write_wheel_with(path: &Path, files: &[(&str, &[u8])]) {
    use std::io::Write;
    let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    for (name, body) in files {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(body).unwrap();
    }
    zw.finish().unwrap();
}

/// A minimal ELF64 little-endian shared object: header, a `PT_LOAD` over the
/// whole file, a `PT_DYNAMIC`, a `.dynamic` with `DT_STRTAB`/`DT_STRSZ` and
/// one `dyn_tag` entry (`DT_RUNPATH` = 29, `DT_RPATH` = 15) naming `value`
/// (`None`: no such entry), and the string table. The string table is
/// mapped at a virtual address unlike its file offset, as in real files.
fn elf_with_dynamic(dyn_tag: u64, value: Option<&str>) -> Vec<u8> {
    const BASE: u64 = 0x40_0000;
    let strtab = format!("\0{}\0", value.unwrap_or(""));
    let (phoff, dynoff, stroff) = (64u64, 64 + 2 * 56, 64 + 2 * 56 + 64);
    let total = stroff + strtab.len() as u64;
    let mut out = vec![0u8; 64];
    out[..4].copy_from_slice(b"\x7fELF");
    out[4] = 2;
    out[5] = 1;
    out[32..40].copy_from_slice(&phoff.to_le_bytes());
    out[54..56].copy_from_slice(&56u16.to_le_bytes());
    out[56..58].copy_from_slice(&2u16.to_le_bytes());
    for (ty, off, vaddr, size) in [(1u32, 0, BASE, total), (2, dynoff, BASE + dynoff, 64)] {
        out.extend(ty.to_le_bytes());
        out.extend(4u32.to_le_bytes());
        for v in [off, vaddr, vaddr, size, size, 8] {
            out.extend(u64::to_le_bytes(v));
        }
    }
    let tag = if value.is_some() { dyn_tag } else { 1 };
    for (t, v) in [
        (5, BASE + stroff),
        (10, strtab.len() as u64),
        (tag, 1),
        (0, 0),
    ] {
        out.extend(u64::to_le_bytes(t));
        out.extend(u64::to_le_bytes(v));
    }
    out.extend(strtab.as_bytes());
    out
}

/// python-triton's shape: `proton`/`proton-viewer` are generated into
/// `/usr/bin` from `entry_points.txt`, and only `[console_scripts]` (and
/// `[gui_scripts]`) name files there — other groups are plugins.
/// A wheel from before `License-Expression`: the free-text header and the
/// trove classifiers are read, `UNKNOWN` and a body line are not a licence.
#[test]
fn wheel_layout_reads_the_legacy_licence_headers() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.whl");
    write_wheel_with(
        &old,
        &[
            ("pkg/__init__.py", b""),
            (
                "pkg-1.0.dist-info/METADATA",
                b"Metadata-Version: 2.1\nName: pkg\nLicense: 3-Clause BSD\n\
                      Classifier: License :: OSI Approved :: BSD License\n\
                      Classifier: Programming Language :: Python\n\nLicense: not a header\n",
            ),
        ],
    );
    let l = wheel_layout(&old).unwrap();
    assert_eq!(l.license_text.as_deref(), Some("3-Clause BSD"));
    assert_eq!(
        l.license_classifiers,
        vec!["License :: OSI Approved :: BSD License"]
    );
    assert_eq!(l.license_expression, None);

    let new = dir.path().join("new.whl");
    write_wheel_with(
        &new,
        &[
            ("pkg/__init__.py", b""),
            (
                "pkg-1.0.dist-info/METADATA",
                b"Name: pkg\nLicense: UNKNOWN\nLicense-Expression: MIT\n\n",
            ),
        ],
    );
    let l = wheel_layout(&new).unwrap();
    assert_eq!(l.license_expression.as_deref(), Some("MIT"));
    assert_eq!(l.license_text, None);
}

#[test]
fn wheel_layout_reads_console_scripts_from_entry_points() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("e.whl");
    write_wheel_with(
        &w,
        &[
            ("pkg/__init__.py", b""),
            (
                "pkg-1.0.dist-info/entry_points.txt",
                b"# generated\n[console_scripts]\nproton = proton.profile:main\n\
                      proton-viewer=proton.viewer:main [viewer]\nproton = dup:main\n\
                      ; skipped\n\n[gui_scripts]\npkg-gui = pkg.gui:run\n\
                      [pkg.plugins]\nnot-a-script = pkg.plug:Plugin\n",
            ),
        ],
    );
    let l = wheel_layout(&w).unwrap();
    assert_eq!(
        l.console_scripts,
        vec!["proton", "proton-viewer", "pkg-gui"]
    );

    let n = dir.path().join("n.whl");
    write_wheel(&n, &["pkg/__init__.py", "pkg-1.0.dist-info/RECORD"]);
    assert!(wheel_layout(&n).unwrap().console_scripts.is_empty());
}

/// Script names go into a generated spec, so anything that is not a plain
/// file name (option-looking, a path, a macro, whitespace) is dropped.
#[test]
fn console_scripts_drop_names_that_are_not_plain_file_names() {
    let text = "[console_scripts]\n--evil = a:b\n../x = a:b\n%{foo} = a:b\n\
                    a b = a:b\n.hidden = a:b\nok.name_1+2-x = a:b\nno-equals\n";
    assert_eq!(
        console_scripts_from_entry_points(text),
        vec!["ok.name_1+2-x"]
    );
}

/// opencv-python-headless' shape: auditwheel's `<name>.libs/` directory of
/// hash-suffixed libraries, and hash-suffixed names found elsewhere.
#[test]
fn wheel_layout_detects_auditwheel_hashed_libs() {
    let dir = tempfile::tempdir().unwrap();
    let in_libs = dir.path().join("a.whl");
    write_wheel(
        &in_libs,
        &[
            "cv2/__init__.py",
            "opencv_python_headless.libs/libavcodec-f34387fd.so.62.11.100",
        ],
    );
    assert!(wheel_layout(&in_libs).unwrap().has_hashed_libs);
    let inline = dir.path().join("b.whl");
    write_wheel(&inline, &["numpy/_core/libopenblasp-r0-37b5f859.3.3.so"]);
    assert!(wheel_layout(&inline).unwrap().has_hashed_libs);
    // Ordinary sonames, a date stamp, and a hash outside a library are not.
    let plain = dir.path().join("c.whl");
    write_wheel(
        &plain,
        &[
            "p/libcuda.so.1",
            "p/libz3.so.4.15",
            "p/libfoo-1.2.so",
            "p/libbar-20240101.so.1",
            "p/data-f34387fd.json",
            "pkg-1.0.dist-info/libx-f34387fd.so.1",
        ],
    );
    assert!(!wheel_layout(&plain).unwrap().has_hashed_libs);
    // The names-only check agrees, and reads no wheel that is not one.
    assert!(wheel_has_hashed_libs(&in_libs));
    assert!(wheel_has_hashed_libs(&inline));
    assert!(!wheel_has_hashed_libs(&plain));
    assert!(!wheel_has_hashed_libs(&dir.path().join("absent.whl")));
}

/// cuda-bindings, nccl4py: no `cuda/__init__.py`, a regular package below it.
#[test]
fn a_namespace_wheel_offers_its_shallowest_regular_package() {
    let dir = tempfile::tempdir().unwrap();
    let ns = dir.path().join("a.whl");
    write_wheel(
        &ns,
        &[
            "cuda/bindings/__init__.py",
            "cuda/bindings/_internal/__init__.py",
            "cuda/bindings/tests/__init__.py",
            "cuda_bindings-1.dist-info/METADATA",
        ],
    );
    let l = wheel_layout(&ns).unwrap();
    assert_eq!(l.importable, None);
    assert_eq!(l.namespace_module.as_deref(), Some("cuda.bindings"));
    // A regular top-level package is the importable name, no namespace needed.
    let reg = dir.path().join("b.whl");
    write_wheel(&reg, &["pkg/__init__.py", "pkg/sub/__init__.py"]);
    let l = wheel_layout(&reg).unwrap();
    assert_eq!(l.importable.as_deref(), Some("pkg"));
    assert_eq!(l.namespace_module, None);
    // Only libraries: nothing to find.
    let libs = dir.path().join("c.whl");
    write_wheel(&libs, &["nvidia/cu13/lib/libx.so.1"]);
    assert_eq!(wheel_layout(&libs).unwrap().namespace_module, None);
}

/// libcusparseLt.so.0 (RUNPATH: $ORIGIN:): found in a `DT_RUNPATH` or a
/// `DT_RPATH`, not for another value, no entry, or a non-ELF file.
/// open-webui's wheel: empty `custom.css`/`loader.js` are listed (rpmlint's
/// `zero-length`), an empty `__init__.py` and dist-info files are not.
#[test]
fn empty_files_are_listed_except_those_rpmlint_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("e.whl");
    write_wheel_with(
        &w,
        &[
            ("pkg/__init__.py", b""),
            ("pkg/static/loader.js", b""),
            ("pkg/static/custom.css", b""),
            ("pkg/static/full.css", b"a{}"),
            ("pkg/py.typed", b""),
            ("pkg-1.dist-info/EMPTY", b""),
        ],
    );
    assert_eq!(
        wheel_layout(&w).unwrap().empty_files,
        ["pkg/static/custom.css", "pkg/static/loader.js"]
    );
}

#[test]
fn origin_runpath_is_read_from_the_dynamic_section() {
    let found = |bytes: &[u8]| elf_has_origin_runpath(std::io::Cursor::new(bytes));
    assert!(found(&elf_with_dynamic(29, Some("$ORIGIN:"))));
    assert!(found(&elf_with_dynamic(15, Some("/opt/x:$ORIGIN/../lib"))));
    assert!(!found(&elf_with_dynamic(29, Some("/opt/lib"))));
    assert!(!found(&elf_with_dynamic(29, None)));

    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("o.whl");
    write_wheel_with(
        &w,
        &[
            ("nvidia/lib/libnotelf.so.1", &[b'x'; 200]),
            (
                "nvidia/lib/libcusparseLt.so.0",
                &elf_with_dynamic(29, Some("$ORIGIN:")),
            ),
        ],
    );
    assert!(wheel_layout(&w).unwrap().origin_runpath);
    let w = dir.path().join("p.whl");
    write_wheel_with(
        &w,
        &[(
            "nvidia/lib/libfoo.so.1",
            &elf_with_dynamic(29, Some("/opt")),
        )],
    );
    assert!(!wheel_layout(&w).unwrap().origin_runpath);
    // A library under .dist-info is not part of the payload.
    let w = dir.path().join("q.whl");
    write_wheel_with(
        &w,
        &[(
            "p-1.dist-info/libx.so",
            &elf_with_dynamic(29, Some("$ORIGIN")),
        )],
    );
    assert!(!wheel_layout(&w).unwrap().origin_runpath);
}

/// Malformed input is `false`, never a panic: every prefix of a valid file,
/// a flipped byte at each position, 32-bit and big-endian classes, absurd
/// offsets and sizes.
#[test]
fn origin_runpath_survives_malformed_elf_files() {
    let good = elf_with_dynamic(29, Some("$ORIGIN:"));
    let found = |bytes: &[u8]| elf_has_origin_runpath(std::io::Cursor::new(bytes));
    assert!(found(&good));
    // The string table starts at 240; cut inside `$ORIGIN` is still a hit.
    for len in 0..good.len() {
        assert_eq!(found(&good[..len]), len >= 248, "truncated to {len}");
    }
    for at in 0..good.len() {
        for flip in [0x00u8, 0xff] {
            let mut bad = good.clone();
            bad[at] = flip;
            let _ = found(&bad);
        }
    }
    let mut bad = good.clone();
    bad[4] = 1; // ELFCLASS32
    assert!(!found(&bad));
    let mut bad = good.clone();
    bad[5] = 2; // big-endian
    assert!(!found(&bad));
    let mut bad = good.clone();
    bad[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // e_phoff
    assert!(!found(&bad));
    let mut bad = good.clone();
    bad[56..58].copy_from_slice(&u16::MAX.to_le_bytes()); // e_phnum
    assert!(!found(&bad));
    // PT_DYNAMIC at an offset far past the end of the file.
    let mut bad = good.clone();
    bad[64 + 56 + 8..64 + 56 + 16].copy_from_slice(&(u64::MAX - 8).to_le_bytes());
    assert!(!found(&bad));
    assert!(!found(b""));
    assert!(!found(&[0x7f; 4096]));
}

#[test]
fn wheel_layout_accepts_a_single_module_wheel() {
    let dir = tempfile::tempdir().unwrap();
    let w = dir.path().join("m.whl");
    write_wheel(&w, &["six.py", "six-1.0.dist-info/METADATA"]);
    assert_eq!(wheel_layout(&w).unwrap().importable.as_deref(), Some("six"));
}
