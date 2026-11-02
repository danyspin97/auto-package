use super::unpack::*;
use std::io::Write;

/// A gzipped tar with one entry per `(name, body)`: `None` is a directory.
/// Names are written byte for byte, so `./x` stays `./x`.
fn tar_gz(entries: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
    let mut ar = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        let mut h = tar::Header::new_old();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        let data: &[u8] = body.unwrap_or(&[]);
        h.set_entry_type(if body.is_some() {
            tar::EntryType::Regular
        } else {
            tar::EntryType::Directory
        });
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append(&h, data).unwrap();
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&ar.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

fn unpack_bytes(bytes: &[u8]) -> (tempfile::TempDir, Result<(), String>) {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    let result = unpack(bytes, Format::TarGz, &dest, LIMITS).map(|_| ());
    (dir, result)
}

#[test]
fn a_dot_slash_name_is_read_under_the_archive_root() {
    // The names are `.` and `src2`, two roots, so nothing is stripped.
    let bytes = tar_gz(&[
        ("./", None),
        ("./build.zig.zon", Some(b"z")),
        ("src2/", None),
        ("src2/x.txt", Some(b"x")),
    ]);
    let (dir, result) = unpack_bytes(&bytes);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("build.zig.zon")).unwrap(), b"z");
    assert_eq!(std::fs::read(out.join("src2/x.txt")).unwrap(), b"x");
}

#[test]
fn a_single_directory_beside_dot_slash_is_not_stripped() {
    // zig names these `.` and `pkg`: two roots, so zig keeps `pkg` as a
    // directory of the tree.
    let bytes = tar_gz(&[
        ("./", None),
        ("pkg/", None),
        ("pkg/build.zig.zon", Some(b"z")),
    ]);
    let (dir, result) = unpack_bytes(&bytes);
    result.unwrap();
    let out = dir.path().join("out");
    assert_eq!(std::fs::read(out.join("pkg/build.zig.zon")).unwrap(), b"z");
    assert!(!out.join("build.zig.zon").exists());
}

#[test]
fn an_archive_under_dot_slash_only_is_refused_as_zig_refuses_it() {
    // zig 0.15.2 fails to unpack this with FileBusy; it is refused here.
    let bytes = tar_gz(&[("./", None), ("./build.zig.zon", Some(b"z"))]);
    let (_dir, result) = unpack_bytes(&bytes);
    let err = result.expect_err("an archive under ./ only must be refused");
    assert!(err.contains("./"), "{err}");
}
