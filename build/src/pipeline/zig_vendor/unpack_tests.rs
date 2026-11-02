use super::unpack::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// One raw tar entry. Names are written byte for byte, so a hostile name such
/// as `/etc/x` or `pkg/../x` reaches the unpacker unchanged.
enum K {
    File(&'static [u8]),
    Exec(&'static [u8]),
    Dir,
    Link(&'static str),
    Hard(&'static str),
    Device,
}

fn raw_tar(entries: &[(&str, K)]) -> Vec<u8> {
    let mut ar = tar::Builder::new(Vec::new());
    for (name, kind) in entries {
        let (typ, data, link, mode): (u8, &[u8], Option<&str>, u32) = match kind {
            K::File(d) => (b'0', d, None, 0o644),
            K::Exec(d) => (b'0', d, None, 0o755),
            K::Dir => (b'5', &[], None, 0o755),
            K::Link(t) => (b'2', &[], Some(t), 0o777),
            K::Hard(t) => (b'1', &[], Some(t), 0o644),
            K::Device => (b'3', &[], None, 0o644),
        };
        let mut h = tar::Header::new_old();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        if let Some(t) = link {
            h.as_old_mut().linkname[..t.len()].copy_from_slice(t.as_bytes());
        }
        h.set_entry_type(tar::EntryType::new(typ));
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        ar.append(&h, data).unwrap();
    }
    ar.into_inner().unwrap()
}

fn tar_gz(entries: &[(&str, K)]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &raw_tar(entries)).unwrap();
    gz.finish().unwrap()
}

/// Unpack `bytes` into a fresh `out` under a temp dir, with `limits`.
fn run(bytes: &[u8], format: Format, limits: Limits) -> (tempfile::TempDir, Result<(), String>) {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    let result = unpack(bytes, format, &dest, limits);
    (dir, result)
}

fn expect_refused(entries: &[(&str, K)], needle: &str) {
    let (_dir, result) = run(&tar_gz(entries), Format::TarGz, LIMITS);
    let err = result.expect_err("the archive must be refused");
    assert!(err.contains(needle), "wanted `{needle}` in: {err}");
}

#[test]
fn files_dirs_and_links_are_written_under_the_top_directory() {
    let bytes = tar_gz(&[
        ("pkg-1.0/", K::Dir),
        ("pkg-1.0/src/", K::Dir),
        ("pkg-1.0/src/main.zig", K::File(b"x")),
        ("pkg-1.0/link", K::Link("src/main.zig")),
        ("pkg-1.0/d2/", K::Dir),
        ("pkg-1.0/d2/link", K::Link("../src/main.zig")),
    ]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("src/main.zig")).unwrap(), b"x");
    assert_eq!(
        std::fs::read_link(out.join("link")).unwrap(),
        Path::new("src/main.zig")
    );
    assert_eq!(
        std::fs::read_link(out.join("d2/link")).unwrap(),
        Path::new("../src/main.zig")
    );
    assert!(!out.join("pkg-1.0").exists());
}

#[test]
fn an_executable_mode_is_kept_and_others_are_not_executable() {
    let bytes = tar_gz(&[
        ("p/", K::Dir),
        ("p/run.sh", K::Exec(b"#!/bin/sh\n")),
        ("p/data", K::File(b"d")),
    ]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    let mode = |f: &str| {
        std::fs::metadata(dir.path().join("out").join(f))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("run.sh"), 0o755);
    assert_eq!(mode("data"), 0o644);
}

#[test]
fn a_zstd_archive_unpacks_like_a_gzip_one() {
    let zst = zstd::stream::encode_all(&raw_tar(&[("p/", K::Dir), ("p/a", K::File(b"a"))])[..], 0)
        .unwrap();
    let (dir, result) = run(&zst, Format::TarZst, LIMITS);
    result.unwrap();
    assert_eq!(std::fs::read(dir.path().join("out/a")).unwrap(), b"a");
}

#[test]
fn absolute_and_climbing_names_are_refused() {
    expect_refused(&[("/etc/x", K::File(b"x"))], "not a relative path");
    expect_refused(
        &[("p/", K::Dir), ("p/../x", K::File(b"x"))],
        "unsafe component",
    );
}

#[test]
fn a_top_level_file_makes_the_top_level_the_root() {
    let bytes = tar_gz(&[
        ("readme", K::File(b"r")),
        ("p/", K::Dir),
        ("p/a", K::File(b"a")),
    ]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("readme")).unwrap(), b"r");
    assert_eq!(std::fs::read(out.join("p/a")).unwrap(), b"a");
}

#[test]
fn a_second_top_level_directory_makes_the_top_level_the_root() {
    let bytes = tar_gz(&[("a/", K::Dir), ("a/x", K::File(b"x")), ("b/", K::Dir)]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("a/x")).unwrap(), b"x");
    assert!(out.join("b").is_dir());
}

#[test]
fn a_single_top_level_link_is_not_taken_as_the_root() {
    let bytes = tar_gz(&[
        ("real/", K::Dir),
        ("real/a", K::File(b"a")),
        ("top", K::Link("real")),
    ]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("real/a")).unwrap(), b"a");
    assert_eq!(
        std::fs::read_link(out.join("top")).unwrap(),
        Path::new("real")
    );
}

#[test]
fn a_link_out_of_a_single_top_directory_is_refused() {
    expect_refused(
        &[
            ("p/", K::Dir),
            ("p/a/", K::Dir),
            ("p/a/l", K::Link("../../x")),
        ],
        "outside the tree",
    );
}

#[test]
fn a_link_out_of_the_top_is_kept_when_the_top_is_not_the_root() {
    let bytes = tar_gz(&[
        ("p/", K::Dir),
        ("p/a/", K::Dir),
        ("p/a/l", K::Link("../../x")),
        ("README", K::File(b"r")),
    ]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    assert_eq!(
        std::fs::read_link(dir.path().join("out/p/a/l")).unwrap(),
        Path::new("../../x")
    );
}

#[test]
fn links_that_leave_the_tree_are_refused() {
    expect_refused(
        &[("p/", K::Dir), ("p/l", K::Link("../../etc/passwd"))],
        "outside the tree",
    );
    expect_refused(
        &[("p/", K::Dir), ("p/l", K::Link("/etc/passwd"))],
        "outside the tree",
    );
    expect_refused(
        &[("p/", K::Dir), ("p/l", K::Link("../x"))],
        "outside the tree",
    );
    expect_refused(&[("p/", K::Dir), ("p/l", K::Link(""))], "no link target");
}

#[test]
fn a_link_through_an_earlier_link_is_refused() {
    expect_refused(
        &[
            ("p/", K::Dir),
            ("p/a", K::Link("b")),
            ("p/c", K::Link("a/../x")),
        ],
        "outside the tree",
    );
}

#[test]
fn a_file_written_through_an_earlier_link_is_refused() {
    expect_refused(
        &[
            ("p/", K::Dir),
            ("p/d", K::Link("e")),
            ("p/d/f", K::File(b"x")),
        ],
        "passes through a link",
    );
}

#[test]
fn hard_links_and_devices_are_refused() {
    expect_refused(&[("p/", K::Dir), ("p/h", K::Hard("p/x"))], "hard link");
    expect_refused(&[("p/", K::Dir), ("p/dev", K::Device)], "device");
}

#[test]
fn too_many_entries_stop_the_unpack() {
    let bytes = tar_gz(&[
        ("p/", K::Dir),
        ("p/a", K::File(b"a")),
        ("p/b", K::File(b"b")),
    ]);
    let (_dir, result) = run(
        &bytes,
        Format::TarGz,
        Limits {
            max_entries: 2,
            ..LIMITS
        },
    );
    assert!(result.unwrap_err().contains("more than 2 entries"));
}

#[test]
fn too_many_file_bytes_stop_the_unpack() {
    let bytes = tar_gz(&[("p/", K::Dir), ("p/a", K::File(b"12345"))]);
    let (_dir, result) = run(
        &bytes,
        Format::TarGz,
        Limits {
            max_bytes: 4,
            ..LIMITS
        },
    );
    assert!(result.unwrap_err().contains("bytes of files"));
}

#[test]
fn too_deep_a_path_is_refused() {
    let bytes = tar_gz(&[("p/", K::Dir), ("p/a/b/c", K::File(b"x"))]);
    let (_dir, result) = run(
        &bytes,
        Format::TarGz,
        Limits {
            max_depth: 2,
            ..LIMITS
        },
    );
    assert!(result.unwrap_err().contains("deeper than 2"));
}

#[test]
fn an_existing_destination_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    std::fs::create_dir(&dest).unwrap();
    let err = unpack(&tar_gz(&[("p/", K::Dir)])[..], Format::TarGz, &dest, LIMITS).unwrap_err();
    assert!(err.contains("already exists"));
}

#[test]
fn an_empty_archive_unpacks_to_an_empty_root_as_zig_does() {
    let bytes = tar_gz(&[]);
    let (dir, result) = run(&bytes, Format::TarGz, LIMITS);
    result.unwrap();
    assert!(
        std::fs::read_dir(dir.path().join("out"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn a_tar_xz_archive_unpacks_like_a_zig_made_one() {
    // `pkg-1.0.tar.xz` as zig 0.15.2 reads it: top directory `pkg-1.0/`.
    let bytes = include_bytes!("testdata/zig-pkg-1.0.tar.xz");
    let (dir, result) = run(&bytes[..], Format::TarXz, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("hello.txt")).unwrap(), b"hello\n");
    assert!(out.join("build.zig.zon").is_file());
}

#[test]
fn a_second_xz_stream_is_not_read_as_zig_does_not() {
    let bytes = include_bytes!("testdata/two-xz-streams.tar.xz");
    let (_dir, result) = run(&bytes[..], Format::TarXz, LIMITS);
    assert!(result.is_err(), "the second stream must not be read");
}

#[test]
fn a_corrupt_xz_archive_is_refused() {
    let mut bytes = include_bytes!("testdata/zig-pkg-1.0.tar.xz").to_vec();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    let (_dir, result) = run(&bytes, Format::TarXz, LIMITS);
    assert!(result.is_err());
}

#[test]
fn only_the_formats_zig_reads_are_read() {
    assert_eq!(Format::of_url("https://x/a-1.0.tar.gz"), Ok(Format::TarGz));
    assert_eq!(Format::of_url("https://x/a.tgz"), Ok(Format::TarGz));
    assert_eq!(Format::of_url("https://x/a.tar.xz"), Ok(Format::TarXz));
    assert_eq!(Format::of_url("https://x/a.txz"), Ok(Format::TarXz));
    assert_eq!(Format::of_url("https://x/a.tar.zst"), Ok(Format::TarZst));
    assert_eq!(Format::of_url("https://x/a.tzst"), Ok(Format::TarZst));
    assert_eq!(Format::of_url("https://x/a-1.0.zip"), Ok(Format::Zip));
    assert!(Format::of_url("https://x/a.tar.bz2").is_err());
    assert!(Format::of_url("https://x/archive").is_err());
}

/// A zip archive of `entries`, written by the zip crate.
fn raw_zip(entries: &[(&str, K)]) -> Vec<u8> {
    use std::io::Write as _;
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, kind) in entries {
        let opts = zip::write::SimpleFileOptions::default();
        match kind {
            K::File(d) => {
                zw.start_file(*name, opts.unix_permissions(0o644)).unwrap();
                zw.write_all(d).unwrap();
            }
            K::Exec(d) => {
                zw.start_file(*name, opts.unix_permissions(0o755)).unwrap();
                zw.write_all(d).unwrap();
            }
            K::Dir => zw.add_directory(*name, opts).unwrap(),
            K::Link(t) => zw.add_symlink(*name, *t, opts).unwrap(),
            K::Hard(_) | K::Device => panic!("a zip archive has no hard links or devices"),
        }
    }
    zw.finish().unwrap().into_inner()
}

fn expect_zip_refused(entries: &[(&str, K)], needle: &str) {
    let (_dir, result) = run(&raw_zip(entries), Format::Zip, LIMITS);
    let err = result.expect_err("the archive must be refused");
    assert!(err.contains(needle), "wanted `{needle}` in: {err}");
}

#[test]
fn a_zig_made_zip_archive_unpacks_like_a_tar_one() {
    // `pkg-1.0.zip` as zig 0.15.2 reads it: top directory `pkg-1.0/`.
    let bytes = include_bytes!("testdata/zig-pkg-1.0.zip");
    let (dir, result) = run(&bytes[..], Format::Zip, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("hello.txt")).unwrap(), b"hello\n");
    assert!(out.join("build.zig.zon").is_file());
    assert!(!out.join("pkg-1.0").exists());
}

#[test]
fn a_zip_archive_keeps_modes_and_the_root_and_writes_links_as_files() {
    let bytes = raw_zip(&[
        ("p/", K::Dir),
        ("p/src/", K::Dir),
        ("p/src/main.zig", K::File(b"x")),
        ("p/run.sh", K::Exec(b"#!/bin/sh\n")),
        ("p/link", K::Link("src/main.zig")),
        ("p/d2/", K::Dir),
        ("p/d2/link", K::Link("../src/main.zig")),
    ]);
    let (dir, result) = run(&bytes, Format::Zip, LIMITS);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("src/main.zig")).unwrap(), b"x");
    // Zig writes a zip's symlink entry as a file holding the target.
    assert!(
        std::fs::symlink_metadata(out.join("link"))
            .unwrap()
            .is_file()
    );
    assert_eq!(std::fs::read(out.join("link")).unwrap(), b"src/main.zig");
    assert_eq!(
        std::fs::read(out.join("d2/link")).unwrap(),
        b"../src/main.zig"
    );
    let mode = |f: &str| std::fs::metadata(out.join(f)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode("run.sh"), 0o755);
    assert_eq!(mode("src/main.zig"), 0o644);
    assert!(!out.join("p").exists());
}

#[test]
fn zip_names_that_are_absolute_or_climb_are_refused() {
    expect_zip_refused(&[("/etc/x", K::File(b"x"))], "not a relative path");
    expect_zip_refused(
        &[("p/", K::Dir), ("p/../x", K::File(b"x"))],
        "unsafe component",
    );
}

#[test]
fn a_zip_symlink_that_leaves_the_tree_is_only_a_file_holding_its_target() {
    // Nothing is linked, so nothing can leave the tree.
    let bytes = raw_zip(&[
        ("p/", K::Dir),
        ("p/a/", K::Dir),
        ("p/a/l", K::Link("../../../etc/passwd")),
    ]);
    let (dir, result) = run(&bytes, Format::Zip, LIMITS);
    result.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("out/a/l")).unwrap(),
        b"../../../etc/passwd"
    );
}

#[test]
fn a_zip_with_too_many_entries_or_bytes_stops() {
    let bytes = raw_zip(&[
        ("p/", K::Dir),
        ("p/a", K::File(b"a")),
        ("p/b", K::File(b"b")),
    ]);
    let (_dir, result) = run(
        &bytes,
        Format::Zip,
        Limits {
            max_entries: 2,
            ..LIMITS
        },
    );
    assert!(result.unwrap_err().contains("more than 2 entries"));
    let bytes = raw_zip(&[("p/", K::Dir), ("p/a", K::File(b"12345"))]);
    let (_dir, result) = run(
        &bytes,
        Format::Zip,
        Limits {
            max_bytes: 4,
            ..LIMITS
        },
    );
    assert!(result.unwrap_err().contains("bytes of files"));
}

#[test]
fn a_truncated_or_corrupt_zip_is_refused() {
    let bytes = include_bytes!("testdata/zig-pkg-1.0.zip");
    let (_dir, result) = run(&bytes[..bytes.len() - 30], Format::Zip, LIMITS);
    assert!(result.is_err());
    let mut corrupt = bytes.to_vec();
    let mid = corrupt.len() / 3;
    corrupt[mid] ^= 0xff;
    let (_dir, result) = run(&corrupt, Format::Zip, LIMITS);
    assert!(result.is_err());
}

#[test]
fn an_entry_expanding_more_than_deflate_can_is_refused() {
    // Deflate expands at most about 1032 to 1, so 1 MiB from 10 bytes is not
    // a real deflate stream of a file.
    assert!(!zip_archive::ratio_ok(1 << 20, 10));
    assert!(zip_archive::ratio_ok(10 << 20, (10 << 20) / 1032));
}
