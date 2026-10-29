use super::*;

enum Member<'a> {
    File(&'a str, &'a [u8]),
    Dir(&'a str),
    Link(&'a str, &'a str),
}

fn write_members(path: &std::path::Path, members: &[Member<'_>]) {
    let file = std::fs::File::create(path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for m in members {
        let mut header = tar::Header::new_gnu();
        match m {
            Member::File(name, body) => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                ar.append_data(&mut header, name, *body).unwrap();
            }
            Member::Dir(name) => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_mode(0o755);
                header.set_cksum();
                ar.append_data(&mut header, name, std::io::empty()).unwrap();
            }
            Member::Link(name, target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                ar.append_link(&mut header, name, target).unwrap();
            }
        }
    }
    ar.into_inner().unwrap().finish().unwrap();
}

/// The chain from the review: `x` is a link that resolves back to the
/// root, so `x/y` is really `y`, and a link created under `x/y` climbs
/// out of the archive. Each step passes the lexical check on its own.
#[test]
fn a_chain_of_links_cannot_redirect_a_later_link_outside() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::write(&outside, b"host secret").unwrap();
    let tarball = dir.path().join("pkg.tar.gz");
    write_members(
        &tarball,
        &[
            Member::Dir("a/"),
            Member::Dir("a/b/"),
            Member::Link("x", "a/b/../.."),
            Member::Dir("x/y/"),
            Member::Link("x/y/link", "../../outside"),
        ],
    );
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();

    let err = extract(&tarball, &root).unwrap_err();

    assert!(matches!(err, Error::LinkEscapes { .. }), "{err}");
    assert!(!root.join("y/link").exists() && !root.join("x/y/link").exists());
    assert_eq!(std::fs::read(&outside).unwrap(), b"host secret");
}

#[test]
fn links_that_stay_inside_the_archive_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("pkg.tar.gz");
    write_members(
        &tarball,
        &[
            Member::Dir("pkg-1/"),
            Member::Dir("pkg-1/sub/"),
            Member::File("pkg-1/lib.so.1", b"elf"),
            Member::Link("pkg-1/lib.so", "lib.so.1"),
            Member::Link("pkg-1/sub/up", "../lib.so"),
            Member::Link("pkg-1/a/d", "../lib.so.1"),
        ],
    );
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();

    extract(&tarball, &root).unwrap();

    assert_eq!(std::fs::read(root.join("pkg-1/sub/up")).unwrap(), b"elf");
}

pub(super) fn write_tar(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        ar.append_data(&mut header, name, *body).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
}

/// A bare `pyproject.toml` at the archive root has no directory above it.
/// It used to panic on the `components[1..len - 1]` slice.
#[test]
fn a_root_level_pyproject_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("bare.tar.gz");
    write_tar(
        &tarball,
        &[("pyproject.toml", b"[project]\nname = \"bare\"\n".as_slice())],
    );
    // Must return rather than panic; the outcome itself is not asserted.
    let _ = inspect(&tarball);
}
