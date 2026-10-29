use super::*;

#[test]
fn the_extraction_estimate_is_six_times_the_archive_within_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("small.tar.gz");
    std::fs::write(&small, b"x").unwrap();
    assert_eq!(extraction_estimate(&small), 64 << 20);
    assert_eq!(extraction_estimate(&dir.path().join("absent")), 64 << 20);
    let big = dir.path().join("big.tar.gz");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(100 << 20)
        .unwrap();
    assert_eq!(
        extraction_estimate(&big),
        (600 << 20).min(max_total_bytes())
    );
    assert!(max_total_bytes() <= 4 << 30);
}

#[test]
fn a_full_disk_is_recognised_by_kind() {
    let full = Error::Io {
        path: PathBuf::new(),
        source: std::io::Error::from(std::io::ErrorKind::StorageFull),
    };
    assert!(full.is_disk_full());
    let no_space = Error::from(crate::space::NotEnoughSpace {
        what: "x".into(),
        needed: 1,
        free: 0,
        reserved: 0,
        margin: 0,
    });
    assert!(no_space.is_disk_full());
    assert!(!Error::LimitExceeded("x".into()).is_disk_full());
}

#[test]
fn copy_entry_is_bounded_by_budget_not_declared_size() {
    // The header claims 5 bytes but the stream holds 100, with only 10
    // bytes of total budget left: the copy must stop and fail.
    let mut b = Budget {
        bytes: MAX_TOTAL_BYTES - 10,
        ..Default::default()
    };
    b.charge(5).unwrap();
    let mut sink = Vec::new();
    let err = b
        .copy_entry(5, &mut std::io::repeat(b'x').take(100), &mut sink)
        .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded(_)));
    assert!(sink.len() <= 11);
    // An honest entry is charged once, not twice.
    let mut b = Budget::default();
    b.charge(5).unwrap();
    b.copy_entry(5, &mut std::io::repeat(b'x').take(5), &mut Vec::new())
        .unwrap();
    assert_eq!(b.bytes, 5);
}

#[test]
fn inspect_enforces_entry_limit() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("many.zip");
    let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("t-1/f{i}")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
    write_zip(&src, &entries);
    assert!(matches!(inspect(&src), Err(Error::LimitExceeded(_))));
}

#[test]
fn root_cmake_beats_nested_setup_py() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.gz");
    write_tar(
        &src,
        &[
            ("lib-1.0/CMakeLists.txt", b"project(lib)\n"),
            (
                "lib-1.0/bindings/python/setup.py",
                b"from setuptools import setup\n",
            ),
            (
                "lib-1.0/vendor/dep/Cargo.toml",
                b"[package]\nname = \"dep\"\n",
            ),
        ],
    );
    let info = inspect(&src).unwrap();
    assert!(info.cmake_manifest);
    assert!(!info.python_packaging);
    assert!(!info.cargo_manifest);
}

#[test]
fn invalid_pyproject_does_not_fail_inspect() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.gz");
    write_tar(
        &src,
        &[
            ("thing-1.0/pyproject.toml", b"[project\nname = = \n"),
            ("thing-1.0/setup.py", b"from setuptools import setup\n"),
        ],
    );
    let info = inspect(&src).unwrap();
    assert!(info.python_packaging);
    assert_eq!(info.project_name, None);
    assert_eq!(
        info.build_system.backend.as_deref(),
        Some("setuptools.build_meta")
    );
}

#[test]
fn dot_slash_prefix_does_not_become_top_level_dir() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.zip");
    write_zip(
        &src,
        &[(
            "./thing-1.0/pyproject.toml",
            b"[project]\nname = \"thing\"\n",
        )],
    );
    let info = inspect(&src).unwrap();
    assert_eq!(info.top_level_dir.as_deref(), Some("thing-1.0"));
    assert_eq!(info.files, vec!["thing-1.0/pyproject.toml".to_string()]);
    assert_eq!(info.project_name.as_deref(), Some("thing"));
}

/// A top-level name that would be written unescaped into the spec must not
/// be taken: newline, RPM macro, whitespace and shell metacharacters.
#[test]
fn unsafe_top_level_names_are_not_taken() {
    for bad in [
        "evil\n%post\nrm -rf /\n-1.0",
        "pct%{name}-1.0",
        "sp ace-1.0",
        "semi;rm-1.0",
        "dollar$(x)-1.0",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("src.tar.gz");
        let member = format!("{bad}/setup.py");
        write_tar(&tarball, &[(member.as_str(), b"x".as_slice())]);
        let info = inspect(&tarball).unwrap();
        assert_eq!(info.top_level_dir, None, "accepted {bad:?}");
    }
}

#[test]
fn is_safe_top_level_dir_accepts_real_source_tree_names() {
    for good in [
        "thing-1.0",
        "MarkupSafe-3.0.3",
        "sqlite3-3",
        "a_b+c~1",
        "v1.2",
        "Pyöpt-1.0",
    ] {
        assert!(is_safe_top_level_dir(good), "{good}");
    }
    for bad in [
        "", ".", "..", "a/b", "a\\b", "a b", "a\nb", "a\u{7}b", "a%b", "a;b", "a$b", "a\"b", "a'b",
        "a`b", "a(b)", "a{b}", "a|b", "a&b", "a<b>", "a*b", "a?b", "a!b", "a#b", "-x", "--option",
        "~root", "~",
    ] {
        assert!(!is_safe_top_level_dir(bad), "{bad:?}");
    }
    // Only a leading `-` or `~` is refused; interior ones are fine.
    for good in ["a-b", "a~b"] {
        assert!(is_safe_top_level_dir(good), "{good}");
    }
}

#[test]
fn budget_rejects_oversized_entries() {
    let mut b = Budget::default();
    assert!(b.charge(10).is_ok());
    assert!(matches!(
        b.charge(MAX_FILE_BYTES + 1),
        Err(Error::LimitExceeded(_))
    ));
    let mut b = Budget {
        entries: MAX_ENTRIES,
        ..Default::default()
    };
    assert!(matches!(b.charge(1), Err(Error::LimitExceeded(_))));
}

#[test]
fn archive_budget_refuses_a_decompression_bomb() {
    // A 1 MiB archive may expand to 64 MiB + 200 MiB, not to gigabytes.
    let mut b = Budget {
        compressed: 1 << 20,
        ..Default::default()
    };
    assert!(b.charge(100 << 20).is_ok());
    assert!(matches!(b.charge(300 << 20), Err(Error::LimitExceeded(_))));
    // An unknown archive size turns the ratio check off.
    let mut unknown = Budget::default();
    assert!(unknown.charge(1 << 30).is_ok());
}

#[cfg(unix)]
#[test]
fn zip_extract_keeps_exec_bit_and_skips_symlinks() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let z = dir.path().join("a.zip");
    let file = std::fs::File::create(&z).unwrap();
    let mut zw = zip::ZipWriter::new(file);
    let o =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zw.start_file("run.sh", o.unix_permissions(0o755)).unwrap();
    zw.write_all(b"#!/bin/sh\n").unwrap();
    zw.add_symlink("link", "/etc/passwd", o).unwrap();
    zw.finish().unwrap();
    let dest = dir.path().join("out");
    extract(&z, &dest).unwrap();
    let mode = std::fs::metadata(dest.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    assert!(std::fs::symlink_metadata(dest.join("link")).is_err());
}

#[test]
fn tar_error_display_includes_cause() {
    let e = Error::Tar(std::io::Error::other("boom"));
    assert!(e.to_string().contains("boom"));
}
