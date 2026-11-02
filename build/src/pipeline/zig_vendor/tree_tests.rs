use super::tree::*;
use std::path::{Path, PathBuf};

/// A tree under a temp dir, with `files` as `(path, content)`.
fn tree(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let at = dir.path().join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, content).unwrap();
    }
    dir
}

fn zon_with_paths(paths: &str) -> Vec<u8> {
    format!(
        ".{{ .name = .hello, .version = \"0.0.1\", .fingerprint = 0x3610a686c4bba334, .paths = .{{ {paths} }} }}\n"
    )
    .into_bytes()
}

/// The tree `zig fetch` hashed for the vectors below: `d/x`, `e/y`, `top`, and
/// the manifest with the given `.paths`.
fn zig_tree(paths: &str) -> tempfile::TempDir {
    tree(&[
        ("build.zig.zon", &zon_with_paths(paths)),
        ("d/x", b"q\n"),
        ("e/y", b"o\n"),
        ("top", b"o\n"),
    ])
}

fn hash_of(dir: &Path, paths: Option<&[&str]>) -> String {
    let paths: Option<Vec<String>> = paths.map(|p| p.iter().map(|s| s.to_string()).collect());
    let items = included_items(dir, &[], paths.as_deref()).unwrap();
    package_hash(&items, 0xc4bb_a334).unwrap()
}

#[test]
fn a_directory_entry_keeps_its_files() {
    let dir = zig_tree("\"d/\"");
    assert_eq!(
        hash_of(dir.path(), Some(&["d/"])),
        "NKO7xAIAAACM-ayVjvxDDRjMVPMtYf-ki93-zWJSQr0k"
    );
    // `zig fetch` gave the same hash with `d`, `./d` and a file entry `d/x`.
    assert_eq!(
        hash_of(dir.path(), Some(&["d"])),
        "NKO7xAIAAACM-ayVjvxDDRjMVPMtYf-ki93-zWJSQr0k"
    );
    assert_eq!(
        hash_of(dir.path(), Some(&["./d"])),
        "NKO7xAIAAACM-ayVjvxDDRjMVPMtYf-ki93-zWJSQr0k"
    );
    assert_eq!(
        hash_of(dir.path(), Some(&["d/x"])),
        "NKO7xAIAAACM-ayVjvxDDRjMVPMtYf-ki93-zWJSQr0k"
    );
}

#[test]
fn an_empty_or_dot_entry_keeps_everything() {
    let dir = zig_tree("\"\"");
    assert_eq!(
        hash_of(dir.path(), Some(&[""])),
        "NKO7xGMAAAA72l-bp8y336vb8S6i6J1tdbq1eb5wBcmV"
    );
    let dot = zig_tree("\".\"");
    assert_eq!(
        hash_of(dot.path(), Some(&["."])),
        "NKO7xGQAAABtSYD5cB1tcH4JCc_NtzBx0H4TdSHbL0dj"
    );
}

#[test]
fn an_entry_matching_nothing_keeps_nothing() {
    let dir = zig_tree("\"d/xy\"");
    assert_eq!(
        hash_of(dir.path(), Some(&["d/xy"])),
        "NKO7xAAAAADjsMRCmPwcFJr79MiZb7kkJ65B5GSbk0yk"
    );
}

#[test]
fn an_absent_paths_field_keeps_everything() {
    let dir = zig_tree("\"\"");
    assert_eq!(
        hash_of(dir.path(), None),
        "NKO7xGMAAAA72l-bp8y336vb8S6i6J1tdbq1eb5wBcmV"
    );
}

#[test]
fn a_link_is_read_and_not_followed() {
    let dir = tree(&[
        (
            "build.zig.zon",
            &zon_with_paths("\"d\", \"build.zig.zon\", \"ln2\""),
        ),
        ("d/x", b"q\n"),
    ]);
    let links = vec![(PathBuf::from("ln2"), PathBuf::from("d/x"))];
    let items = included_items(
        dir.path(),
        &links,
        Some(&[
            "d".to_string(),
            "build.zig.zon".to_string(),
            "ln2".to_string(),
        ]),
    )
    .unwrap();
    assert!(
        items
            .iter()
            .any(|i| matches!(i, Item::Link { rel, target } if rel == "ln2" && target == b"d/x"))
    );
    // Same tree as zig 0.15.2 hashed it: `ln2` is a link to `d/x`.
    assert_eq!(
        package_hash(&items, 0xc4bb_a334).unwrap(),
        "NKO7xHgAAADM3s0hyILG-RzkntYIoQwgW1IAQCklI7T5"
    );
}

#[test]
fn a_link_on_disk_is_refused_since_unpacking_never_writes_one() {
    let dir = tree(&[("d/x", b"q\n")]);
    std::os::unix::fs::symlink("d/x", dir.path().join("ln2")).unwrap();
    let err = included_items(dir.path(), &[], None).unwrap_err();
    assert!(err.contains("ln2 is a link on disk"), "{err}");
}

#[test]
fn an_executable_bit_is_reported_but_not_hashed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tree(&[("run.sh", b"#!/bin/sh\n"), ("data", b"d")]);
    std::fs::set_permissions(
        dir.path().join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let items = included_items(dir.path(), &[], None).unwrap();
    let exec = |rel: &str| {
        items.iter().find_map(|i| match i {
            Item::File { rel: r, exec, .. } if r == rel => Some(*exec),
            _ => None,
        })
    };
    assert_eq!(exec("run.sh"), Some(true));
    assert_eq!(exec("data"), Some(false));
}

#[test]
fn unsafe_paths_entries_are_refused() {
    let dir = zig_tree("\"\"");
    assert!(included_items(dir.path(), &[], Some(&["/etc".to_string()])).is_err());
    assert!(included_items(dir.path(), &[], Some(&["../x".to_string()])).is_err());
}

#[test]
fn a_paths_entry_names_a_file_or_a_directory_prefix() {
    let keep = |paths: &[&str], rel: &str| {
        let paths: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        path_kept(Some(&paths), rel)
    };
    assert!(path_kept(None, "anything"));
    assert!(keep(&["d"], "d/x"));
    assert!(!keep(&["d"], "dx"));
    assert!(!keep(&["d/x"], "d/xy"));
    assert!(keep(&["./d/"], "d/x"));
    assert!(keep(&[""], "any/where"));
}
