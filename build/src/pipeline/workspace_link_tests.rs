use super::*;

/// Decompressed bytes of a `.tar.gz`, for looking inside a repacked sdist.
fn gunzip(path: &Path) -> Vec<u8> {
    let mut raw = Vec::new();
    let file = std::fs::File::open(path).unwrap();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(file), &mut raw).unwrap();
    raw
}

const HOST_SECRET: &[u8] = b"HOST-SECRET-BYTES";

/// A sdist whose workspace needs fixing and which also carries a symlink
/// to a file outside the archive. The fix must never copy that file's
/// bytes into the tarball it rewrites.
#[test]
fn workspace_fix_does_not_copy_a_host_file_through_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, HOST_SECRET).unwrap();
    let tarball = dir.path().join("pkg-1.0.tar.gz");
    {
        let file = std::fs::File::create(&tarball).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        let manifest = b"[workspace]\nmembers = [\"gone\"]\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(manifest.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "pkg-1.0/Cargo.toml", &manifest[..])
            .unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        ar.append_link(&mut h, "pkg-1.0/leak", &secret).unwrap();
        ar.into_inner().unwrap().finish().unwrap();
    }

    // The fix may refuse the archive; either way the host file stays out.
    let _ = fix_cargo_workspace_in_tarball(&tarball, "pkg");

    let raw = gunzip(&tarball);
    assert!(!raw.windows(HOST_SECRET.len()).any(|w| w == HOST_SECRET));
}

/// Repacking records a link as a link. Following it would copy the host
/// file into the archive.
#[test]
fn repacking_records_a_link_instead_of_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, HOST_SECRET).unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("pkg-1.0")).unwrap();
    std::fs::write(tree.join("pkg-1.0/Cargo.toml"), "[package]\n").unwrap();
    std::os::unix::fs::symlink(&secret, tree.join("pkg-1.0/leak")).unwrap();
    let tarball = dir.path().join("out.tar.gz");
    std::fs::write(&tarball, b"stale").unwrap();

    repack_tarball(&tree, &tarball).unwrap();

    let raw = gunzip(&tarball);
    assert!(!raw.windows(HOST_SECRET.len()).any(|w| w == HOST_SECRET));
    let file = std::fs::File::open(&tarball).unwrap();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let leak = ar
        .entries()
        .unwrap()
        .map(|e| e.unwrap())
        .find(|e| e.path().unwrap().ends_with("leak"))
        .expect("link entry is packed");
    assert!(leak.header().entry_type().is_symlink());
}
