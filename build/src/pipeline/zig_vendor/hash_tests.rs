use super::hash::*;
use super::zon;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Hash a `.tar.gz` dependency the way the fetch step will: strip its top
/// directory, hash the regular files and links, and take the id from the
/// fingerprint of its own `build.zig.zon`.
fn archive_hash(name: &str) -> (String, zon::Manifest) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/pipeline/zig_vendor/testdata")
        .join(name);
    let file = std::fs::File::open(path).unwrap();
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut hash = PackageHash::new();
    let mut manifest = None;
    for entry in ar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let rel: PathBuf = entry.path().unwrap().components().skip(1).collect();
        if rel.as_os_str().is_empty() || entry.header().entry_type().is_dir() {
            continue;
        }
        let rel = rel.to_string_lossy().into_owned();
        if entry.header().entry_type().is_symlink() {
            let target = entry.link_name().unwrap().unwrap().into_owned();
            hash.link(&rel, target.as_os_str().as_encoded_bytes());
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if rel == "build.zig.zon" {
            manifest = Some(zon::parse(std::str::from_utf8(&bytes).unwrap()).unwrap());
        }
        hash.file(&rel, &mut bytes.as_slice()).unwrap();
    }
    let manifest = manifest.expect("the archive has a build.zig.zon");
    let id = manifest.fingerprint.expect("a fingerprint") as u32;
    (hash.finish(id).unwrap(), manifest)
}

#[test]
fn pixman_archive_hashes_to_its_declared_hash() {
    let (digest, m) = archive_hash("pixman-0.3.0.tar.gz");
    assert_eq!(m.version.as_deref(), Some("0.3.0"));
    let declared = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX";
    assert_eq!(
        split_declared(declared).unwrap(),
        ("pixman-0.3.0", digest.as_str())
    );
}

#[test]
fn wayland_archive_hashes_to_its_declared_hash() {
    // The digest contains `-`, which the split must not treat as a separator.
    let (digest, _) = archive_hash("wayland-0.6.0.tar.gz");
    let declared = "wayland-0.6.0-lQa1kqz8AQADQmdNJsNhLoNHcnEGEUjrOaPV-dtEnEmX";
    assert_eq!(
        split_declared(declared).unwrap(),
        ("wayland-0.6.0", digest.as_str())
    );
}

#[test]
fn a_changed_byte_or_path_changes_the_hash() {
    let pack = |content: &[u8], path: &str| {
        let mut hash = PackageHash::new();
        hash.file(path, &mut &content[..]).unwrap();
        hash.finish(7).unwrap()
    };
    let base = pack(b"abc", "a");
    assert_ne!(pack(b"abd", "a"), base);
    assert_ne!(pack(b"abc", "b"), base);
    assert_eq!(pack(b"abc", "a"), base);
}

#[test]
fn the_id_is_the_first_four_bytes_and_the_size_the_next_four() {
    let mut hash = PackageHash::new();
    hash.file("a", &mut &b"xyz"[..]).unwrap();
    let bytes = decode_for_test(&hash.finish(0x0403_0201).unwrap());
    assert_eq!(&bytes[..4], &[1, 2, 3, 4]);
    assert_eq!(&bytes[4..8], &3u32.to_le_bytes());
}

/// Undo [`encode`] for a 44-character string, for the byte-layout test.
fn decode_for_test(s: &str) -> Vec<u8> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let vals: Vec<u32> = s
        .bytes()
        .map(|c| alphabet.iter().position(|&a| a == c).unwrap() as u32)
        .collect();
    let mut out = Vec::new();
    for q in vals.chunks(4) {
        let n = q
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, v)| acc | (v << (18 - 6 * i)));
        for i in 0..3 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    out
}

#[test]
fn zig_0_15_2_hashes_a_tree_with_a_symlink() {
    // `zig fetch` on this tree (paths: d, build.zig.zon, ln2) printed the hash
    // below. The link is `ln2 -> d/x`.
    let zon = b".{ .name = .hello, .version = \"0.0.1\", .fingerprint = 0x3610a686c4bba334, .paths = .{ \"d\", \"build.zig.zon\", \"ln2\" } }\n";
    let mut hash = PackageHash::new();
    hash.file("build.zig.zon", &mut &zon[..]).unwrap();
    hash.file("d/x", &mut &b"q\n"[..]).unwrap();
    hash.link("ln2", b"d/x");
    assert_eq!(
        hash.finish(0xc4bb_a334).unwrap(),
        "NKO7xHgAAADM3s0hyILG-RzkntYIoQwgW1IAQCklI7T5"
    );
}

#[test]
fn zig_0_15_2_hashes_files_with_an_executable_bit_the_same() {
    // `zig fetch` on this tree printed the hash below. `run.sh` was mode 0755
    // in one run and 0644 in another, and both gave this hash.
    let zon = b".{ .name = .hello, .version = \"0.0.1\", .fingerprint = 0x3610a686c4bba334, .paths = .{ \"run.sh\", \"sub\", \"link\", \"build.zig.zon\" } }\n";
    let mut hash = PackageHash::new();
    hash.file("build.zig.zon", &mut &zon[..]).unwrap();
    hash.file("run.sh", &mut &b"#!/bin/sh\necho hi\n"[..])
        .unwrap();
    hash.file("sub/a-b", &mut &b"data\n"[..]).unwrap();
    hash.file("sub/z", &mut &b"z"[..]).unwrap();
    assert_eq!(
        hash.finish(0xc4bb_a334).unwrap(),
        "NKO7xJsAAAA_-zRS9YSNzw0zwXzXBQVOonZRBR5rkDhW"
    );
}

#[test]
fn an_empty_package_has_zigs_empty_hash() {
    // `zig fetch` on a tree whose `.paths` matched nothing.
    assert_eq!(
        PackageHash::new().finish(0xc4bb_a334).unwrap(),
        "NKO7xAAAAADjsMRCmPwcFJr79MiZb7kkJ65B5GSbk0yk"
    );
}

#[test]
fn the_order_of_files_does_not_matter() {
    let mut a = PackageHash::new();
    a.file("x", &mut &b"1"[..]).unwrap();
    a.file("y/z", &mut &b"2"[..]).unwrap();
    let mut b = PackageHash::new();
    b.file("y/z", &mut &b"2"[..]).unwrap();
    b.file("x", &mut &b"1"[..]).unwrap();
    assert_eq!(a.finish(7).unwrap(), b.finish(7).unwrap());
}

#[test]
fn base64_is_url_safe_and_unpadded() {
    assert_eq!(encode(&[0xfb, 0xff]), "-_8");
    assert_eq!(encode(&[0u8]), "AA");
    assert_eq!(encode(&[0u8; 33]), "A".repeat(44));
    assert_eq!(encode(&[0xff; 33]).len(), 44);
}

#[test]
fn a_declared_hash_splits_at_its_last_44_characters() {
    let (nv, digest) =
        split_declared("wlroots-0.20.2-dev-jmOlcrJuBABMWPGDKK86BO7zrfmxm17nimqFb0gffZTn").unwrap();
    assert_eq!(nv, "wlroots-0.20.2-dev");
    assert_eq!(digest.len(), DIGEST_LEN);
    assert_eq!(digest, "jmOlcrJuBABMWPGDKK86BO7zrfmxm17nimqFb0gffZTn");
}

/// A plain tar in testdata, as its files and links, with the top directory
/// stripped, for the hashes zig printed for it.
fn tree_of_tar(name: &str) -> PackageHash {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/pipeline/zig_vendor/testdata")
        .join(name);
    let mut ar = tar::Archive::new(std::fs::File::open(path).unwrap());
    let mut hash = PackageHash::new();
    for entry in ar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let rel: PathBuf = entry.path().unwrap().components().skip(1).collect();
        if rel.as_os_str().is_empty() || entry.header().entry_type().is_dir() {
            continue;
        }
        let rel = rel.to_string_lossy().into_owned();
        if entry.header().entry_type().is_symlink() {
            let target = entry.link_name().unwrap().unwrap().into_owned();
            hash.link(&rel, target.as_os_str().as_encoded_bytes());
        } else {
            hash.file(&rel, &mut entry).unwrap();
        }
    }
    hash
}

#[test]
fn zig_0_15_2_verifies_the_legacy_1220_hashes() {
    // Each is the hash zig 0.15.2 printed as "the fetched package has" for the
    // same tree, when a dependency declared another hash.
    assert_eq!(
        tree_of_tar("zig-pkg-1.0.tar").finish_legacy(),
        "122011a6ed00c7abd2e8cbc0a9b267c4b6441dda9a3386a7c341284fe9e036628064"
    );
    // Nested directories and an executable file.
    assert_eq!(
        tree_of_tar("zig-pkg-2.0.tar").finish_legacy(),
        "1220b74845688f8019f40acfcc1f232dc3ccc7f1772cc28c30e2e3ae8459729170e8"
    );
    // A symlink, hashed as its path and target.
    assert_eq!(
        tree_of_tar("zig-pkg-3.0.tar").finish_legacy(),
        "122045b9cbb315c89cd20446f8c89c4f28b113cf74d1870c22eb28e7acc9030f13a0"
    );
}

#[test]
fn zig_0_15_2_hashes_the_nested_tree_in_the_current_form() {
    // `zig fetch` on the same tree printed this hash for the current form.
    assert_eq!(
        tree_of_tar("zig-pkg-2.0.tar").finish(0x3030_ff30).unwrap(),
        "MP8wMCwCAAC3SEVoj4AZ9ArPzB8jLcPMx_F3LMKMMOLj"
    );
}

#[test]
fn a_legacy_hash_is_told_apart_from_the_current_form() {
    let old = format!("1220{}", "ab".repeat(32));
    assert_eq!(parse_declared(&old), Ok(Declared::Legacy));
    // Not the legacy form: a wrong length, or upper-case hex.
    assert!(parse_declared(&format!("1220{}", "ab".repeat(31))).is_err());
    assert!(split_declared(&old).is_err());
    assert!(parse_declared(&format!("1220{}", "AB".repeat(32))).is_err());
}

#[test]
fn malformed_and_old_hashes_are_refused() {
    assert!(split_declared("short-1.0-abc").is_err());
    assert!(split_declared("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());
    let no_dash = format!("pkg1.0{}", "A".repeat(44));
    assert!(split_declared(&no_dash).is_err());
    let bad_char = format!("pkg-1.0-{}!", "A".repeat(43));
    assert!(split_declared(&bad_char).is_err());
    assert!(split_declared(&format!("-{}", "A".repeat(44))).is_err());
}

#[test]
fn a_name_with_a_path_or_an_odd_character_is_refused() {
    // The name becomes a directory in vendor/p, so only [A-Za-z0-9._+-] is
    // allowed in it.
    let digest = "A".repeat(44);
    for name in ["a/b-1.0", "../x-1.0", "sp ace-1.0", "q\"-1.0", "x:y-1.0"] {
        let declared = format!("{name}-{digest}");
        let err = split_declared(&declared).unwrap_err();
        assert!(err.contains("outside [A-Za-z0-9._+-]"), "{declared}: {err}");
    }
    assert!(split_declared(&format!("pkg_x+y.z-1.0.0-rc.1-{digest}")).is_ok());
}
