use super::*;

// --- Archive-format dispatch ------------------------------------------
//
// Each format gets a tiny single-file fixture, extracted into a tempdir,
// and the extracted file's contents are checked byte-for-byte. The
// fixtures intentionally use the same file name across formats so a
// single `expect_extracted` helper verifies everything after extraction.

fn write_tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut buf);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            ar.append_data(&mut header, name, *body).unwrap();
        }
        ar.finish().unwrap();
    }
    buf
}

fn write_tar_bz2(path: &Path, entries: &[(&str, &[u8])]) {
    let tar_bytes = write_tar_bytes(entries);
    let file = std::fs::File::create(path).unwrap();
    let mut enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::fast());
    std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
    enc.finish().unwrap();
}

fn write_tar_zst(path: &Path, entries: &[(&str, &[u8])]) {
    let tar_bytes = write_tar_bytes(entries);
    let file = std::fs::File::create(path).unwrap();
    let mut enc = zstd::Encoder::new(file, 3).unwrap();
    std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
    enc.finish().unwrap();
}

pub(super) fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write;
    let file = std::fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in entries {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(body).unwrap();
    }
    zw.finish().unwrap();
}

/// Every extraction test shares the same fixture: a one-file archive
/// whose extracted path and body are known in advance.
fn make_fixture_entries() -> &'static [(&'static str, &'static [u8])] {
    &[("thing-1.0/README", b"hello\n")]
}

fn assert_extracted(dest: &Path) {
    let p = dest.join("thing-1.0").join("README");
    let body = std::fs::read(&p).expect("extracted file exists");
    assert_eq!(body, b"hello\n");
}

#[test]
fn extract_tar_gz() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.gz");
    write_tar(&src, make_fixture_entries());
    let out = dir.path().join("out");
    extract(&src, &out).unwrap();
    assert_extracted(&out);
}

#[test]
fn extract_refuses_a_hard_link_through_a_symlinked_directory() {
    let build = |hard_target: &str| {
        let mut buf = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buf);
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            ar.append_link(&mut h, "pkg/d", "sub").unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, "pkg/sub/f", b"x".as_slice())
                .unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Link);
            h.set_size(0);
            h.set_mode(0o644);
            ar.append_link(&mut h, "pkg/h", hard_target).unwrap();
            ar.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let tgz = dir.path().join("a.tar.gz");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut enc, &buf).unwrap();
        std::fs::write(&tgz, enc.finish().unwrap()).unwrap();
        let dest = dir.path().join("out");
        (extract(&tgz, &dest), dir, dest)
    };
    let (result, _dir, _) = build("pkg/d/f");
    assert!(
        matches!(result, Err(Error::LinkEscapes { .. })),
        "{result:?}"
    );
    let (result, _dir, dest) = build("pkg/sub/f");
    result.unwrap();
    assert!(dest.join("pkg/h").is_file());
}

#[test]
fn extract_skips_links_that_leave_the_root() {
    let mut buf = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut buf);
        for (name, target) in [
            ("pkg/abs", "/etc/passwd"),
            ("pkg/up", "../../outside"),
            ("pkg/ok", "file"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            ar.append_link(&mut h, name, target).unwrap();
        }
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "pkg/file", b"x".as_slice()).unwrap();
        ar.finish().unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let tgz = dir.path().join("a.tar.gz");
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &buf).unwrap();
    std::fs::write(&tgz, enc.finish().unwrap()).unwrap();
    let dest = dir.path().join("out");
    extract(&tgz, &dest).unwrap();
    assert!(std::fs::symlink_metadata(dest.join("pkg/abs")).is_err());
    assert!(std::fs::symlink_metadata(dest.join("pkg/up")).is_err());
    assert!(std::fs::symlink_metadata(dest.join("pkg/ok")).is_ok());
}

#[test]
fn extract_where_writes_only_the_kept_entries() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.gz");
    write_tar(
        &src,
        &[
            ("thing-1.0/README", b"hello\n"),
            ("thing-1.0/setup.py", b"print(1)\n"),
        ],
    );
    let out = dir.path().join("out");
    extract_where(&src, &out, |p| p.ends_with("setup.py")).unwrap();
    assert!(out.join("thing-1.0").join("setup.py").is_file());
    assert!(!out.join("thing-1.0").join("README").exists());
}

#[test]
fn filtered_out_entries_still_count_against_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("bomb.tar.zst");
    // zstd packs zeros about 10000:1, far past the allowed expansion.
    let big = vec![0u8; 100 << 20];
    write_tar_zst(
        &src,
        &[("thing-1.0/big.bin", &big), ("thing-1.0/setup.py", b"x")],
    );
    let out = dir.path().join("out");
    let err = extract_where(&src, &out, |p| p.ends_with("setup.py")).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded(_)), "{err}");
}

#[test]
fn bounded_scan_reads_a_normal_archive_and_stops_at_the_byte_cap() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.tar.gz");
    write_tar(&src, &[("t/a", b"1"), ("t/b", b"2")]);
    let mut ar = bounded_tar_gz(std::fs::File::open(&src).unwrap());
    let names: Vec<_> = ar
        .entries()
        .unwrap()
        .take(SCAN_MAX_ENTRIES)
        .flatten()
        .map(|e| e.path().unwrap().into_owned())
        .collect();
    assert_eq!(names, [Path::new("t/a"), Path::new("t/b")]);
    // The reader is cut after the cap: a stream longer than it is truncated.
    let mut zeros = flate2::read::GzDecoder::new(std::io::Cursor::new({
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &vec![0u8; 1 << 20]).unwrap();
        enc.finish().unwrap()
    }))
    .take(1000);
    let mut sink = Vec::new();
    zeros.read_to_end(&mut sink).unwrap();
    assert_eq!(sink.len(), 1000);
}

#[test]
fn extract_tar_bz2() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.bz2");
    write_tar_bz2(&src, make_fixture_entries());
    let out = dir.path().join("out");
    extract(&src, &out).unwrap();
    assert_extracted(&out);
}

#[test]
fn extract_tar_zst() {
    // The Rust ap_service emits .tar.zst — this is the format the review
    // specifically flagged as needing extraction support.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.zst");
    write_tar_zst(&src, make_fixture_entries());
    let out = dir.path().join("out");
    extract(&src, &out).unwrap();
    assert_extracted(&out);
}

#[test]
fn extract_zip_sdist() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.zip");
    write_zip(&src, make_fixture_entries());
    let out = dir.path().join("out");
    extract(&src, &out).unwrap();
    assert_extracted(&out);
}

#[test]
fn extract_wheel_still_works() {
    // Same container as .zip, but with the `.whl` role — must take the
    // same code path and continue to extract without the system `unzip`
    // binary (which the old implementation shelled out to).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("foo-1.0-py3-none-any.whl");
    write_zip(&src, &[("foo-1.0.dist-info/METADATA", b"Name: foo\n")]);
    let out = dir.path().join("out");
    extract(&src, &out).unwrap();
    let body = std::fs::read(out.join("foo-1.0.dist-info/METADATA")).unwrap();
    assert_eq!(body, b"Name: foo\n");
}

#[test]
fn extract_refuses_unrecognised_extension() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("thing.rar");
    std::fs::write(&src, b"not an archive").unwrap();
    let out = dir.path().join("out");
    let err = extract(&src, &out).expect_err("rar is not supported");
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
}

#[test]
fn failed_extract_leaves_neither_dest_nor_partial_behind() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("broken.tar.gz");
    std::fs::write(&src, b"not gzip at all").unwrap();
    let out = dir.path().join("sdist");
    assert!(extract(&src, &out).is_err());
    assert!(!out.exists());
    assert!(!dir.path().join("sdist.partial").exists());
}

// --- Inspection across formats ----------------------------------------

#[test]
fn inspect_tar_zst() {
    // The real motivation: a .tar.zst used to be fed to GzDecoder and
    // inspection reported no manifests, so an ap_service tarball looked
    // like an empty archive.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.zst");
    write_tar_zst(
        &src,
        &[("thing-1.0/pyproject.toml", b"[project]\nname = \"thing\"\n")],
    );
    let info = inspect(&src).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("thing"));
    assert!(info.python_packaging);
}

#[test]
fn inspect_tar_bz2() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.bz2");
    write_tar_bz2(
        &src,
        &[("thing-1.0/pyproject.toml", b"[project]\nname = \"thing\"\n")],
    );
    let info = inspect(&src).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("thing"));
}

#[test]
fn inspect_zip_sdist() {
    // A .zip sdist is a legacy shape that PyPI still accepts — inspection
    // has to look inside it the same way it does for tarballs.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.zip");
    write_zip(
        &src,
        &[("thing-1.0/pyproject.toml", b"[project]\nname = \"thing\"\n")],
    );
    let info = inspect(&src).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("thing"));
    assert!(info.python_packaging);
}

#[test]
fn non_utf8_manifest_does_not_fail_inspect() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.tar.gz");
    write_tar(
        &src,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[project]\nname = \"thing\"\n# \xff\xfe\n".as_slice(),
            ),
            ("thing-1.0/vendor/Cargo.toml", b"\xff\xff".as_slice()),
        ],
    );
    let info = inspect(&src).unwrap();
    assert_eq!(info.project_name.as_deref(), Some("thing"));
}

#[test]
fn malformed_poetry_table_does_not_fail_parse() {
    let raw = "[tool.poetry]\nname = 5\nversion = [1]\n[tool.other]\nx = 1\n";
    let p: PyprojectToml = toml::from_str(raw).unwrap();
    assert_eq!(p.declared_name(), None);
    let raw = "[tool.poetry]\nname = \"pp\"\n";
    let p: PyprojectToml = toml::from_str(raw).unwrap();
    assert_eq!(p.declared_name().as_deref(), Some("pp"));
}
