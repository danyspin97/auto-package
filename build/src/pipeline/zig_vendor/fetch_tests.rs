use super::fetch::*;
use super::hash::PackageHash;
use super::zon;
use std::cell::RefCell;

/// The real pixman archive; its declared hash is in river's `build.zig.zon`.
const PIXMAN: &[u8] = include_bytes!("testdata/pixman-0.3.0.tar.gz");
const PIXMAN_HASH: &str = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX";
/// The real wayland archive, used where a different archive is needed.
const WAYLAND: &[u8] = include_bytes!("testdata/wayland-0.6.0.tar.gz");
const PIXMAN_URL: &str = "https://codeberg.org/ifreund/zig-pixman/archive/v0.3.0.tar.gz";
pub(super) const HEX40: &str = "c183af18ae35dd5fb3dde709697c8f84bb1dc7f3";

pub(super) enum Route {
    Redirect(String),
    /// A body served as `application/octet-stream`, as codeberg and gitlab
    /// serve their archives.
    Body(Vec<u8>),
    /// A body served with this Content-Type.
    Typed(&'static str, Vec<u8>),
    /// A body served with no Content-Type at all.
    Untyped(Vec<u8>),
}

/// Serves `routes` by exact URL and records every URL asked for.
pub(super) struct Fake {
    routes: Vec<(String, Route)>,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    pub(super) fn new(routes: Vec<(&str, Route)>) -> Self {
        Fake {
            routes: routes
                .into_iter()
                .map(|(u, r)| (u.to_string(), r))
                .collect(),
            calls: RefCell::new(Vec::new()),
        }
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Fetch for Fake {
    fn get(&self, url: &str) -> Result<Response, String> {
        self.calls.borrow_mut().push(url.to_string());
        match self.routes.iter().find(|(u, _)| u == url) {
            Some((_, Route::Redirect(to))) => Ok(Response::Redirect(to.clone())),
            Some((_, Route::Body(b))) => Ok(Response::Body {
                content_type: Some("application/octet-stream".to_string()),
                body: Box::new(std::io::Cursor::new(b.clone())),
            }),
            Some((_, Route::Untyped(b))) => Ok(Response::Body {
                content_type: None,
                body: Box::new(std::io::Cursor::new(b.clone())),
            }),
            Some((_, Route::Typed(ct, b))) => Ok(Response::Body {
                content_type: Some(ct.to_string()),
                body: Box::new(std::io::Cursor::new(b.clone())),
            }),
            None => Err(format!("no route for {url}")),
        }
    }
}

fn tar_gz(top: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    dir.set_cksum();
    ar.append_data(&mut dir, format!("{top}/"), &[][..])
        .unwrap();
    for (path, content) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, format!("{top}/{path}"), content.as_slice())
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap()
}

/// A dependency archive whose manifest declares `deps` (zon struct fields).
/// `unhashed` files are archived but outside `.paths`, so zig never hashes
/// them. Returns the archive and the hash the manifest declares for it.
fn archive(
    name: &str,
    fingerprint: u64,
    deps: &str,
    unhashed: &[(&str, &[u8])],
) -> (Vec<u8>, String) {
    let zon = format!(
        ".{{ .name = .{name}, .version = \"0.1.0\", .fingerprint = {fingerprint:#x}, \
         .paths = .{{ \"build.zig.zon\", \"src\" }}, .dependencies = .{{ {deps} }} }}\n"
    );
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("build.zig.zon".to_string(), zon.into_bytes()),
        (
            "src/lib.zig".to_string(),
            format!("// {name}\n").into_bytes(),
        ),
    ];
    let mut hash = PackageHash::new();
    for (p, c) in &files {
        hash.file(p, &mut c.as_slice()).unwrap();
    }
    let digest = hash.finish((fingerprint & 0xffff_ffff) as u32).unwrap();
    for (p, c) in unhashed {
        files.push((p.to_string(), c.to_vec()));
    }
    (
        tar_gz(&format!("{name}-0.1.0"), &files),
        format!("{name}-0.1.0-{digest}"),
    )
}

pub(super) fn root(deps: &str) -> zon::Manifest {
    zon::parse(&format!(
        ".{{ .name = .root, .version = \"1.0.0\", .fingerprint = 0x3610a686c4bba334, \
         .dependencies = .{{ {deps} }} }}"
    ))
    .unwrap()
}

pub(super) fn dep(name: &str, url: &str, hash: &str) -> String {
    format!(".{name} = .{{ .url = \"{url}\", .hash = \"{hash}\" }},")
}

pub(super) struct Quiet;
impl ap_core::sink::Reporter for Quiet {
    fn report(&self, _: &ap_core::event::Event) {}
}

/// Fetch `root` with `fake`, unpacking under a fresh temp dir.
pub(super) fn fetch(
    fake: &Fake,
    package_root: &zon::Manifest,
) -> (tempfile::TempDir, Result<Vec<Dep>, String>) {
    crate::workspace::disable_disk_gate_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let req = Request {
        package: "pkg",
        fetch: fake,
        work: dir.path(),
        limits: super::unpack::LIMITS,
        reporter: &Quiet,
    };
    let result = fetch_vendor(&req, package_root);
    (dir, result)
}

#[test]
fn a_real_archive_is_fetched_and_its_hash_checked() {
    let fake = Fake::new(vec![(PIXMAN_URL, Route::Body(PIXMAN.to_vec()))]);
    let m = root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH));
    let (_dir, result) = fetch(&fake, &m);
    let deps = result.unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].declared, PIXMAN_HASH);
    assert_eq!(deps[0].manifest.name.as_deref(), Some("pixman"));
    assert!(!deps[0].items.is_empty());
    assert_eq!(fake.calls(), [PIXMAN_URL]);
}

/// `pkg-1.0.tar.xz` made for zig 0.15.2 (`xz` of its `pkg-1.0.tar`), and the
/// hash `zig fetch` printed for it.
const ZIG_XZ: &[u8] = include_bytes!("testdata/zig-pkg-1.0.tar.xz");
const ZIG_XZ_HASH: &str = "pkg-1.0.0-adtcAfUAAAARpu0Ax6vS6MvAqbJnxLZEHdqaM4anw0Eo";
const ZIG_XZ_URL: &str = "https://codeberg.org/x/pkg/archive/v1.0.tar.xz";
/// `pkg-1.0.zip`, and `pkg-1.0` with a symlink in its `.paths`, both as zig
/// 0.15.2 printed their hashes.
const ZIG_ZIP: &[u8] = include_bytes!("testdata/zig-pkg-1.0.zip");
const ZIG_LINKY_ZIP: &[u8] = include_bytes!("testdata/zig-linky.zip");
const ZIG_LINKY_HASH: &str = "pkg-1.0.0-adtcAQoBAADqzx2aY9slcvj2hMtkPNekQNx1xATI8ZnD";

#[test]
fn a_tar_xz_dependency_is_fetched_and_its_zig_hash_checked() {
    let fake = Fake::new(vec![(ZIG_XZ_URL, Route::Body(ZIG_XZ.to_vec()))]);
    let m = root(&dep("pkg", ZIG_XZ_URL, ZIG_XZ_HASH));
    let (_dir, result) = fetch(&fake, &m);
    let deps = result.unwrap();
    assert_eq!(deps[0].declared, ZIG_XZ_HASH);
    assert_eq!(fake.calls(), [ZIG_XZ_URL]);
}

#[test]
fn a_content_type_names_the_format_over_the_url_extension() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0.zip";
    let fake = Fake::new(vec![(
        url,
        Route::Typed("application/x-xz", ZIG_XZ.to_vec()),
    )]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_XZ_HASH)));
    assert_eq!(result.unwrap()[0].declared, ZIG_XZ_HASH);
}

#[test]
fn a_url_without_an_extension_takes_its_content_type() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0";
    let fake = Fake::new(vec![(
        url,
        Route::Typed("application/zip", ZIG_ZIP.to_vec()),
    )]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_XZ_HASH)));
    assert_eq!(result.unwrap()[0].declared, ZIG_XZ_HASH);
}

#[test]
fn an_unrecognized_content_type_is_refused_after_the_request() {
    let fake = Fake::new(vec![(
        ZIG_XZ_URL,
        Route::Typed("text/html", ZIG_XZ.to_vec()),
    )]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", ZIG_XZ_URL, ZIG_XZ_HASH)));
    assert!(result.unwrap_err().contains("unrecognized Content-Type"));
    assert_eq!(fake.calls(), [ZIG_XZ_URL]);
}

#[test]
fn a_bz2_archive_is_refused_because_zig_refuses_it() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0.tar.bz2";
    let fake = Fake::new(vec![(url, Route::Body(ZIG_XZ.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_XZ_HASH)));
    assert!(
        result
            .unwrap_err()
            .contains("not a format zig_vendor reads")
    );
}

#[test]
fn a_query_and_an_upper_case_extension_are_read_and_a_fragment_is_not_sent() {
    let url = "https://codeberg.org/x/pkg/archive/V1.0.TAR.XZ?download=1";
    let fake = Fake::new(vec![(url, Route::Body(ZIG_XZ.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_XZ_HASH)));
    assert_eq!(result.unwrap()[0].declared, ZIG_XZ_HASH);
    assert_eq!(fake.calls(), [url]);
    let frag = "https://codeberg.org/x/pkg/archive/v1.0.tar.xz#frag";
    let plain = "https://codeberg.org/x/pkg/archive/v1.0.tar.xz";
    let fake = Fake::new(vec![(plain, Route::Body(ZIG_XZ.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", frag, ZIG_XZ_HASH)));
    assert_eq!(result.unwrap()[0].declared, ZIG_XZ_HASH);
    assert_eq!(fake.calls(), [plain]);
}

/// `pkg-1.0.tar` as zig 0.15.2 made it, and the legacy `1220` hash zig printed
/// for it.
const ZIG_TAR: &[u8] = include_bytes!("testdata/zig-pkg-1.0.tar");
const ZIG_TAR_LEGACY: &str = "122011a6ed00c7abd2e8cbc0a9b267c4b6441dda9a3386a7c341284fe9e036628064";

#[test]
fn a_legacy_1220_dependency_is_fetched_and_its_hash_checked() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0.tar";
    let fake = Fake::new(vec![(url, Route::Body(ZIG_TAR.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_TAR_LEGACY)));
    assert_eq!(result.unwrap()[0].declared, ZIG_TAR_LEGACY);
}

#[test]
fn a_legacy_1220_hash_that_does_not_match_is_refused() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0.tar";
    let wrong = format!("1220{}", "00".repeat(32));
    let fake = Fake::new(vec![(url, Route::Body(ZIG_TAR.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, &wrong)));
    let err = result.unwrap_err();
    assert!(
        err.contains("archive hash 1220") && err.contains(&wrong),
        "{err}"
    );
}

#[test]
fn a_git_url_is_fetched_as_its_forge_archive_of_that_commit() {
    let git = format!("git+https://codeberg.org/ifreund/zig-pixman#{HEX40}");
    let archive_url = format!("https://codeberg.org/ifreund/zig-pixman/archive/{HEX40}.tar.gz");
    let fake = Fake::new(vec![(&archive_url, Route::Body(PIXMAN.to_vec()))]);
    let m = root(&dep("pixman", &git, PIXMAN_HASH));
    let (_dir, result) = fetch(&fake, &m);
    let deps = result.unwrap();
    assert_eq!(deps[0].url, git);
    assert_eq!(deps[0].archive_url, archive_url);
    assert_eq!(fake.calls(), [archive_url]);
}

#[test]
fn a_host_off_the_allowlist_is_refused_and_never_requested() {
    let url = "https://evil.example.net/pixman-0.3.0.tar.gz";
    let fake = Fake::new(vec![(url, Route::Body(PIXMAN.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", url, PIXMAN_HASH)));
    let err = result.unwrap_err();
    assert!(err.contains("evil.example.net"), "{err}");
    assert!(fake.calls().is_empty());
}

#[test]
fn a_redirect_to_an_allowed_host_is_followed() {
    let fake = Fake::new(vec![
        (
            PIXMAN_URL,
            Route::Redirect("https://codeberg.org/x/final.tar.gz".into()),
        ),
        (
            "https://codeberg.org/x/final.tar.gz",
            Route::Body(PIXMAN.to_vec()),
        ),
    ]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)));
    result.unwrap();
    assert_eq!(
        fake.calls(),
        [PIXMAN_URL, "https://codeberg.org/x/final.tar.gz"]
    );
}

#[test]
fn a_relative_redirect_stays_on_the_same_host() {
    let fake = Fake::new(vec![
        (PIXMAN_URL, Route::Redirect("/x/final.tar.gz".into())),
        (
            "https://codeberg.org/x/final.tar.gz",
            Route::Body(PIXMAN.to_vec()),
        ),
    ]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)));
    result.unwrap();
}

#[test]
fn a_redirect_to_an_unlisted_host_is_refused_before_it_is_requested() {
    let fake = Fake::new(vec![(
        PIXMAN_URL,
        Route::Redirect("https://evil.example.net/a.tar.gz".into()),
    )]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)));
    assert!(result.unwrap_err().contains("evil.example.net"));
    assert_eq!(fake.calls(), [PIXMAN_URL]);
}

#[test]
fn a_redirect_to_plain_http_is_refused() {
    let fake = Fake::new(vec![(
        PIXMAN_URL,
        Route::Redirect("http://codeberg.org/x.tar.gz".into()),
    )]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)));
    assert!(result.unwrap_err().contains("not an https://"));
}

#[test]
fn a_redirect_loop_stops_after_the_limit() {
    let fake = Fake::new(vec![(PIXMAN_URL, Route::Redirect(PIXMAN_URL.into()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)));
    assert!(result.unwrap_err().contains("redirects"));
    assert_eq!(fake.calls().len(), MAX_REDIRECTS + 1);
}

#[test]
fn an_archive_with_another_hash_is_refused_with_both_hashes() {
    let fake = Fake::new(vec![(PIXMAN_URL, Route::Body(WAYLAND.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, PIXMAN_HASH)));
    let err = result.unwrap_err();
    assert!(err.contains("archive hash"), "{err}");
    assert!(err.contains("≠ build.zig.zon"), "{err}");
    assert!(err.contains(PIXMAN_HASH), "{err}");
}

#[test]
fn a_dependency_of_a_dependency_is_fetched_from_its_own_manifest() {
    let (b_archive, b_hash) = archive("bee", 0x1111_2222_3333_4444, "", &[]);
    let b_url = "https://codeberg.org/o/bee/archive/v1.tar.gz";
    let (a_archive, a_hash) = archive(
        "ay",
        0x5555_6666_7777_8888,
        &dep("bee", b_url, &b_hash),
        &[],
    );
    let a_url = "https://codeberg.org/o/ay/archive/v1.tar.gz";
    let fake = Fake::new(vec![
        (a_url, Route::Body(a_archive)),
        (b_url, Route::Body(b_archive)),
    ]);
    let (_dir, result) = fetch(&fake, &root(&dep("ay", a_url, &a_hash)));
    let deps = result.unwrap();
    let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"ay") && names.contains(&"bee"));
    assert_eq!(fake.calls().len(), 2);
}

#[test]
fn files_outside_paths_do_not_change_the_hash() {
    let (with_extra, declared) = archive("pk", 0x9999, "", &[("docs/readme", b"not hashed")]);
    let (without, same) = archive("pk", 0x9999, "", &[]);
    assert_eq!(declared, same);
    for archive in [with_extra, without] {
        let url = "https://codeberg.org/o/pk/archive/v1.tar.gz";
        let fake = Fake::new(vec![(url, Route::Body(archive))]);
        let (_dir, result) = fetch(&fake, &root(&dep("pk", url, &declared)));
        result.unwrap();
    }
}

#[test]
fn a_dependency_without_a_hash_is_refused() {
    let url = PIXMAN_URL;
    let m = root(&format!(".pixman = .{{ .url = \"{url}\" }},"));
    let fake = Fake::new(vec![]);
    let (_dir, result) = fetch(&fake, &m);
    assert!(result.unwrap_err().contains("no .hash"));
    assert!(fake.calls().is_empty());
}

#[test]
fn a_path_dependency_is_skipped() {
    let m = root(".local = .{ .path = \"../local\" },");
    let fake = Fake::new(vec![]);
    let (_dir, result) = fetch(&fake, &m);
    assert!(result.unwrap().is_empty());
    assert!(fake.calls().is_empty());
}

#[test]
fn a_zip_dependency_is_fetched_and_its_zig_hash_checked() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0.zip";
    let fake = Fake::new(vec![(url, Route::Body(ZIG_ZIP.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_XZ_HASH)));
    assert_eq!(result.unwrap()[0].declared, ZIG_XZ_HASH);
    assert_eq!(fake.calls(), [url]);
}

#[test]
fn a_zip_with_a_symlink_has_the_hash_zig_gives_it() {
    let url = "https://codeberg.org/x/pkg/archive/v1.0.zip";
    let fake = Fake::new(vec![(url, Route::Body(ZIG_LINKY_ZIP.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pkg", url, ZIG_LINKY_HASH)));
    assert_eq!(result.unwrap()[0].declared, ZIG_LINKY_HASH);
}

#[test]
fn an_archive_without_a_manifest_is_refused() {
    let no_zon = tar_gz("pk-1", &[("src/lib.zig".to_string(), b"x".to_vec())]);
    let url = "https://codeberg.org/o/pk/archive/v1.tar.gz";
    let fake = Fake::new(vec![(url, Route::Body(no_zon))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pk", url, PIXMAN_HASH)));
    assert!(result.unwrap_err().contains("no build.zig.zon"));
}

#[test]
fn forge_git_urls_map_to_their_commit_archive() {
    let c = HEX40;
    let cases = [
        (
            format!("git+https://codeberg.org/ifreund/zig-wlroots#{c}"),
            format!("https://codeberg.org/ifreund/zig-wlroots/archive/{c}.tar.gz"),
        ),
        (
            format!("git+https://codeberg.org/ifreund/translate-c?ref=0.16.x#{c}"),
            format!("https://codeberg.org/ifreund/translate-c/archive/{c}.tar.gz"),
        ),
        (
            format!("git+https://github.com/o/r#{c}"),
            format!("https://codeload.github.com/o/r/tar.gz/{c}"),
        ),
        (
            format!("git+https://gitlab.com/o/r#{c}"),
            format!("https://gitlab.com/o/r/-/archive/{c}/r-{c}.tar.gz"),
        ),
        (
            format!("git+https://git.sr.ht/~o/r#{c}"),
            format!("https://git.sr.ht/~o/r/archive/{c}.tar.gz"),
        ),
    ];
    for (git, want) in cases {
        assert_eq!(archive_url(&git).unwrap().0, want, "{git}");
    }
}

#[test]
fn git_urls_that_are_not_pinned_or_not_forges_are_refused() {
    let c = HEX40;
    let refused = [
        format!("git+https://example.org/o/r#{c}"),
        format!("git+https://codeberg.org/o/r"),
        "git+https://codeberg.org/o/r#v0.3.0".to_string(),
        format!("git+https://codeberg.org/o/r#{}", &c[..12]),
        format!("git+https://codeberg.org/o/r?depth=1#{c}"),
        format!("git+https://codeberg.org/o/r?ref=a&x=1#{c}"),
        format!("git+https://codeberg.org/o/../x#{c}"),
        format!("git+https://codeberg.org/o/r/extra#{c}"),
        format!("git+https://codeberg.org/o r#{c}"),
        format!("git+http://codeberg.org/o/r#{c}"),
        format!("git+ssh://codeberg.org/o/r#{c}"),
        format!("git+https://git.sr.ht/o/r#{c}"),
        "http://codeberg.org/o/r/archive/v1.tar.gz".to_string(),
    ];
    for url in refused {
        assert!(archive_url(&url).is_err(), "{url} should be refused");
    }
}

#[test]
fn a_malformed_declared_hash_is_refused_before_any_request() {
    let fake = Fake::new(vec![(PIXMAN_URL, Route::Body(PIXMAN.to_vec()))]);
    let (_dir, result) = fetch(&fake, &root(&dep("pixman", PIXMAN_URL, "1220abcdef")));
    assert!(result.unwrap_err().contains("zig dep pixman"));
    assert!(fake.calls().is_empty());
}
