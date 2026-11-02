use super::fetch_tests::{Fake, HEX40, Route, dep, fetch, root};
use super::tree::Item;

/// The `build.zig.zon` of the escaping-links fixture, byte for byte as zig
/// 0.15.2 was given it.
const ESCAPE_ZON: &[u8] = b".{ .name = .pkg, .version = \"1.0.0\", .fingerprint = 0xfe6ff7d5e56618e7, .minimum_zig_version = \"0.15.0\", .dependencies = .{}, .paths = .{ \"\" } }\n";
const ESCAPE_URL: &str = "https://codeberg.org/x/pkg/archive/v1.0.0.tar.gz";
/// The hash `zig fetch` (0.15.2) printed for `escaping_links_archive`.
const ESCAPE_HASH: &str = "pkg-1.0.0-5xhm5acAAAAShCPB01y6rmDUnKP0_AiaChK3-jKQDNic";

/// The archive given to `zig fetch`: `pkg/` with a manifest and a file, a
/// link that climbs out of it, and an absolute link.
fn escaping_links_archive() -> Vec<u8> {
    let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    dir.set_cksum();
    ar.append_data(&mut dir, "pkg/", &[][..]).unwrap();
    for (path, content) in [
        ("pkg/build.zig.zon", ESCAPE_ZON),
        ("pkg/a.zig", b"pub fn main() void {}\n" as &[u8]),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, path, content).unwrap();
    }
    for (path, target) in [
        ("pkg/sub/evil", "../../../../../../etc/passwd"),
        ("pkg/abs", "/etc/passwd"),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_mode(0o777);
        h.set_size(0);
        ar.append_link(&mut h, path, target).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap()
}

#[test]
fn links_that_leave_the_tree_hash_as_zig_hashes_them_and_are_not_written() {
    let fake = Fake::new(vec![(ESCAPE_URL, Route::Body(escaping_links_archive()))]);
    let m = root(&dep("pkg", ESCAPE_URL, ESCAPE_HASH));
    let (_dir, result) = fetch(&fake, &m);
    let deps = result.unwrap();
    assert_eq!(deps[0].declared, ESCAPE_HASH);
    assert!(deps[0].root.join("a.zig").is_file());
    assert!(std::fs::symlink_metadata(deps[0].root.join("sub/evil")).is_err());
    assert!(std::fs::symlink_metadata(deps[0].root.join("abs")).is_err());
    assert!(deps[0].items.iter().any(|i| matches!(
        i,
        Item::Link { rel, target } if rel == "sub/evil" && target == b"../../../../../../etc/passwd"
    )));
}

#[test]
fn each_forge_archive_is_read_as_the_content_type_it_is_served_with() {
    // The archive each forge serves for a commit, and the Content-Type it
    // sends: codeberg and gitlab send octet-stream, which the URL's extension
    // decides; sr.ht sends application/gzip; codeload sends application/x-gzip
    // for an extensionless URL, and that type alone names the format.
    let cases = [
        (
            format!("git+https://codeberg.org/x/pkg#{HEX40}"),
            format!("https://codeberg.org/x/pkg/archive/{HEX40}.tar.gz"),
            "application/octet-stream",
        ),
        (
            format!("git+https://gitlab.com/x/pkg#{HEX40}"),
            format!("https://gitlab.com/x/pkg/-/archive/{HEX40}/pkg-{HEX40}.tar.gz"),
            "application/octet-stream",
        ),
        (
            format!("git+https://git.sr.ht/~x/pkg#{HEX40}"),
            format!("https://git.sr.ht/~x/pkg/archive/{HEX40}.tar.gz"),
            "application/gzip",
        ),
        (
            format!("git+https://github.com/x/pkg#{HEX40}"),
            format!("https://codeload.github.com/x/pkg/tar.gz/{HEX40}"),
            "application/x-gzip",
        ),
    ];
    for (git, archive_url, content_type) in cases {
        let fake = Fake::new(vec![(
            archive_url.as_str(),
            Route::Typed(content_type, escaping_links_archive()),
        )]);
        let (_dir, result) = fetch(&fake, &root(&dep("pkg", &git, ESCAPE_HASH)));
        assert_eq!(result.unwrap()[0].declared, ESCAPE_HASH, "{git}");
        assert_eq!(fake.calls(), [archive_url]);
    }
}

#[test]
fn a_response_with_no_content_type_is_refused_as_zig_refuses_it() {
    // zig 0.15.2 fetching a `.tar.gz` URL from a local server that sends no
    // Content-Type: `error: missing 'Content-Type' header`.
    let url = "https://codeberg.org/x/pkg/archive/v1.0.0.tar.gz";
    let fake = Fake::new(vec![(url, Route::Untyped(escaping_links_archive()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ESCAPE_HASH)));
    let err = result.unwrap_err();
    assert!(err.msg.contains("no Content-Type"), "{err}");
}
