use super::fetch::Dep;
use super::hash::split_declared;
use super::tree::{included_items, package_hash};
use super::unpack::{Format, LIMITS, unpack};
use super::vendor_tar::{Layout, write_vendor_tar_zst};

/// The archive for `deps` in `layout`, in memory, with scratch files in a
/// temporary directory.
fn vendor_tar_zst(deps: &[Dep], layout: Layout) -> Result<Vec<u8>, String> {
    let scratch = tempfile::tempdir().unwrap();
    let mut out = Vec::new();
    write_vendor_tar_zst(deps, layout, scratch.path(), &mut out)?;
    Ok(out)
}

#[test]
fn a_file_gets_the_same_bytes_as_memory() {
    let (dir, dep) = pixman_dep();
    let in_memory = vendor_tar_zst(std::slice::from_ref(&dep), Layout::Packed).unwrap();
    let path = dir.path().join("vendor.tar.zst");
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    write_vendor_tar_zst(std::slice::from_ref(&dep), Layout::Packed, dir.path(), file).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), in_memory);
}
use super::zon::Manifest;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const PIXMAN: &[u8] = include_bytes!("testdata/pixman-0.3.0.tar.gz");
const PIXMAN_HASH: &str = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX";
/// Pixman's fingerprint is 0x16aa7fe79f4c292c; its zig id is the low 32 bits.
const PIXMAN_ID: u32 = 0x9f4c_292c;

/// One entry read back from a tar.
#[derive(Debug)]
struct Got {
    path: String,
    is_dir: bool,
    is_link: bool,
    mode: u32,
    uid: u64,
    gid: u64,
    mtime: u64,
    link: Option<String>,
    data: Vec<u8>,
}

fn read_tar(r: impl Read) -> Vec<Got> {
    let mut ar = tar::Archive::new(r);
    let mut out = Vec::new();
    for entry in ar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let h = entry.header().clone();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let link = entry
            .link_name()
            .unwrap()
            .map(|p| p.to_string_lossy().into_owned());
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        out.push(Got {
            path,
            is_dir: h.entry_type().is_dir(),
            is_link: h.entry_type().is_symlink(),
            mode: h.mode().unwrap(),
            uid: h.uid().unwrap(),
            gid: h.gid().unwrap(),
            mtime: h.mtime().unwrap(),
            link,
            data,
        });
    }
    out
}

/// The entries of a `vendor.tar.zst`.
fn outer(bytes: &[u8]) -> Vec<Got> {
    read_tar(zstd::stream::read::Decoder::new(bytes).unwrap())
}

/// The inner `p/<hash>.tar.gz` entry of `entries`, read back.
fn inner(entries: &[Got], declared: &str) -> Vec<Got> {
    let gz = entries
        .iter()
        .find(|e| e.path == format!("vendor/p/{declared}.tar.gz"))
        .expect("the packed archive");
    read_tar(flate2::read::GzDecoder::new(&gz.data[..]))
}

/// Unpack `bytes` into a fresh dir, returning it with the dep's own tree.
fn pixman_dep() -> (tempfile::TempDir, Dep) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pixman");
    let links = unpack(PIXMAN, Format::TarGz, &root, LIMITS).unwrap();
    let text = std::fs::read_to_string(root.join("build.zig.zon")).unwrap();
    let manifest = super::zon::parse(&text).unwrap();
    let items = included_items(&root, &links, manifest.paths.as_deref()).unwrap();
    let dep = Dep {
        name: "pixman".into(),
        declared: PIXMAN_HASH.into(),
        url: String::new(),
        archive_url: String::new(),
        root,
        manifest,
        items,
    };
    (dir, dep)
}

/// A dependency built from `files` (path, content, mode) and `links`
/// (path, target), with an empty manifest.
fn synthetic(
    declared: &str,
    files: &[(&str, &[u8], u32)],
    links: &[(&str, &str)],
) -> (tempfile::TempDir, Dep) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    for (rel, data, mode) in files {
        let at = root.join(rel);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, data).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(*mode)).unwrap();
    }
    let links: Vec<(PathBuf, PathBuf)> = links
        .iter()
        .map(|(rel, target)| (PathBuf::from(rel), PathBuf::from(target)))
        .collect();
    let items = included_items(&root, &links, None).unwrap();
    let dep = Dep {
        name: "x".into(),
        declared: declared.into(),
        url: String::new(),
        archive_url: String::new(),
        root,
        manifest: Manifest::default(),
        items,
    };
    (dir, dep)
}

#[test]
fn the_packed_layout_writes_one_tar_gz_per_dependency() {
    let (_dir, dep) = pixman_dep();
    let bytes = vendor_tar_zst(&[dep], Layout::Packed).unwrap();
    let entries = outer(&bytes);
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "vendor",
            "vendor/p",
            &format!("vendor/p/{PIXMAN_HASH}.tar.gz")
        ]
        .map(|s| s.to_string())
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
    );
    // Inside: `<hash>/` and the files, with no `vendor/p/<hash>/` directory.
    let members = inner(&entries, PIXMAN_HASH);
    assert_eq!(members[0].path, PIXMAN_HASH);
    assert!(members[0].is_dir);
    assert!(
        members
            .iter()
            .all(|m| m.path == PIXMAN_HASH || m.path.starts_with(&format!("{PIXMAN_HASH}/")))
    );
    assert!(
        members
            .iter()
            .any(|m| m.path == format!("{PIXMAN_HASH}/pixman.zig"))
    );
}

#[test]
fn the_unpacked_layout_writes_directories_and_no_tar_gz() {
    let (_dir, dep) = pixman_dep();
    let on_disk = std::fs::read(dep.root.join("pixman.zig")).unwrap();
    let bytes = vendor_tar_zst(&[dep], Layout::Unpacked).unwrap();
    let entries = outer(&bytes);
    assert!(entries.iter().all(|e| !e.path.ends_with(".tar.gz")));
    let dir = format!("vendor/p/{PIXMAN_HASH}");
    assert!(entries.iter().any(|e| e.path == dir && e.is_dir));
    let zig = entries
        .iter()
        .find(|e| e.path == format!("{dir}/pixman.zig"))
        .expect("the source file");
    assert!(!zig.is_dir);
    assert_eq!(zig.data, on_disk);
}

#[test]
fn the_layout_follows_the_zig_version() {
    assert_eq!(Layout::for_zig("0.16.0"), Ok(Layout::Packed));
    assert_eq!(Layout::for_zig("0.16.0-dev.5+abc"), Ok(Layout::Packed));
    assert_eq!(Layout::for_zig("1.0.0"), Ok(Layout::Packed));
    assert_eq!(Layout::for_zig("0.15.2"), Ok(Layout::Unpacked));
    assert_eq!(Layout::for_zig("0.14.1"), Ok(Layout::Unpacked));
    assert!(Layout::for_zig("").is_err());
    assert!(Layout::for_zig("zig").is_err());
    assert!(Layout::for_zig("0").is_err());
}

#[test]
fn the_packed_archive_hashes_back_to_its_declared_hash() {
    let (_dir, dep) = pixman_dep();
    let bytes = vendor_tar_zst(&[dep], Layout::Packed).unwrap();
    let entries = outer(&bytes);
    let gz = entries
        .iter()
        .find(|e| e.path.ends_with(".tar.gz"))
        .unwrap();
    let back = tempfile::tempdir().unwrap();
    tar::Archive::new(flate2::read::GzDecoder::new(&gz.data[..]))
        .unpack(back.path())
        .unwrap();
    let tree = back.path().join(PIXMAN_HASH);
    let items = included_items(&tree, &[], None).unwrap();
    let (_, digest) = split_declared(PIXMAN_HASH).unwrap();
    assert_eq!(package_hash(&items, PIXMAN_ID).unwrap(), digest);
}

#[test]
fn the_output_does_not_depend_on_file_times_or_the_order_given() {
    let (_dir, dep) = pixman_dep();
    let first = vendor_tar_zst(std::slice::from_ref(&dep), Layout::Packed).unwrap();
    let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567_890);
    for item in &dep.items {
        if let super::tree::Item::File { abs, .. } = item {
            std::fs::File::options()
                .write(true)
                .open(abs)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    }
    let (_dir2, other) = pixman_dep();
    let second = vendor_tar_zst(&[other], Layout::Packed).unwrap();
    assert_eq!(first, second);
    let (_dir3, a) = synthetic("a-1.0-A", &[("x", b"1", 0o644)], &[]);
    let (_dir4, b) = synthetic("b-1.0-B", &[("y", b"2", 0o644)], &[]);
    let ab = vendor_tar_zst(&[a, b], Layout::Unpacked).unwrap();
    let (_dir5, a2) = synthetic("a-1.0-A", &[("x", b"1", 0o644)], &[]);
    let (_dir6, b2) = synthetic("b-1.0-B", &[("y", b"2", 0o644)], &[]);
    let ba = vendor_tar_zst(&[b2, a2], Layout::Unpacked).unwrap();
    assert_eq!(ab, ba);
}

#[test]
fn the_entries_carry_fixed_owner_time_and_mode() {
    let (_dir, dep) = synthetic(
        "p-1.0-P",
        &[
            ("run.sh", b"#!/bin/sh\n", 0o755),
            ("src/a.zig", b"a", 0o600),
        ],
        &[],
    );
    let bytes = vendor_tar_zst(&[dep], Layout::Unpacked).unwrap();
    for e in outer(&bytes) {
        assert_eq!((e.uid, e.gid, e.mtime), (0, 0, 0), "{}", e.path);
        if e.is_dir {
            assert_eq!(e.mode, 0o755, "{}", e.path);
        }
    }
    let by = |p: &str| outer(&bytes).into_iter().find(|e| e.path == p).unwrap();
    assert_eq!(by("vendor/p/p-1.0-P/run.sh").mode, 0o755);
    assert_eq!(by("vendor/p/p-1.0-P/src/a.zig").mode, 0o644);
}

#[test]
fn directories_above_each_file_are_written() {
    let (_dir, dep) = synthetic("d-1.0-D", &[("src/deep/x.zig", b"x", 0o644)], &[]);
    let bytes = vendor_tar_zst(&[dep], Layout::Unpacked).unwrap();
    let paths: Vec<String> = outer(&bytes).into_iter().map(|e| e.path).collect();
    for want in [
        "vendor/p/d-1.0-D/src",
        "vendor/p/d-1.0-D/src/deep",
        "vendor/p/d-1.0-D/src/deep/x.zig",
    ] {
        assert!(
            paths.iter().any(|p| p.trim_end_matches('/') == want),
            "{want} in {paths:?}"
        );
    }
}

#[test]
fn links_are_written_as_links_in_both_layouts() {
    let (_dir, dep) = synthetic("l-1.0-L", &[("d/x", b"q", 0o644)], &[("ln", "d/x")]);
    let packed = outer(&vendor_tar_zst(std::slice::from_ref(&dep), Layout::Packed).unwrap());
    let inner_links: Vec<Got> = inner(&packed, "l-1.0-L")
        .into_iter()
        .filter(|e| e.is_link)
        .collect();
    assert_eq!(inner_links.len(), 1);
    assert_eq!(inner_links[0].path, "l-1.0-L/ln");
    assert_eq!(inner_links[0].link.as_deref(), Some("d/x"));

    let unpacked = outer(&vendor_tar_zst(&[dep], Layout::Unpacked).unwrap());
    let ln = unpacked
        .iter()
        .find(|e| e.path == "vendor/p/l-1.0-L/ln")
        .unwrap();
    assert!(ln.is_link);
    assert_eq!(ln.link.as_deref(), Some("d/x"));
}

#[test]
fn two_dependencies_with_one_hash_are_refused() {
    let (_a, one) = synthetic("same-1.0-S", &[("x", b"1", 0o644)], &[]);
    let (_b, two) = synthetic("same-1.0-S", &[("x", b"2", 0o644)], &[]);
    let err = vendor_tar_zst(&[one, two], Layout::Packed).unwrap_err();
    assert!(err.contains("same-1.0-S"), "{err}");
}

#[test]
fn an_empty_list_writes_only_the_two_directories() {
    let entries = outer(&vendor_tar_zst(&[], Layout::Packed).unwrap());
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["vendor", "vendor/p"]);
}
