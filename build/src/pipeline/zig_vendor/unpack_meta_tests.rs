use super::unpack::*;
use std::io::Write;

/// The tar bytes of `entries`, each `(kind, name, body)`, gzipped.
fn tar_gz(entries: &[(tar::EntryType, &str, Vec<u8>)]) -> Vec<u8> {
    let mut ar = tar::Builder::new(Vec::new());
    for (kind, name, body) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(*kind);
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, body.as_slice()).unwrap();
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
fn a_gnu_long_name_over_the_metadata_limit_is_refused_before_it_is_read() {
    // The body is the name of the next entry; it is far over the limit.
    let big = vec![b'a'; 1 << 20];
    let bytes = tar_gz(&[
        (tar::EntryType::GNULongName, "././@LongLink", big),
        (tar::EntryType::Regular, "pkg/x", b"x".to_vec()),
    ]);
    let (_dir, result) = unpack_bytes(&bytes);
    let err = result.expect_err("the long name must be refused");
    assert!(err.contains("metadata entry"), "{err}");
    assert!(err.contains("over the"), "{err}");
}

#[test]
fn a_pax_header_over_the_metadata_limit_is_refused_before_it_is_read() {
    let big = b"30 path=pkg/x\n".repeat(1 << 16);
    let bytes = tar_gz(&[
        (tar::EntryType::XHeader, "PaxHeaders/x", big),
        (tar::EntryType::Regular, "pkg/x", b"x".to_vec()),
    ]);
    let (_dir, result) = unpack_bytes(&bytes);
    let err = result.expect_err("the PAX header must be refused");
    assert!(err.contains("over the"), "{err}");
}

#[test]
fn a_long_name_under_the_limit_is_read_as_before() {
    // Over 100 bytes, so the writer uses a GNU long name for it.
    let name = format!("pkg/{}/x", "d".repeat(120));
    let bytes = tar_gz(&[(tar::EntryType::Regular, &name, b"x".to_vec())]);
    let (dir, result) = unpack_bytes(&bytes);
    result.unwrap();
    let rel = format!("{}/x", "d".repeat(120));
    assert_eq!(
        std::fs::read(dir.path().join("out").join(rel)).unwrap(),
        b"x"
    );
}
