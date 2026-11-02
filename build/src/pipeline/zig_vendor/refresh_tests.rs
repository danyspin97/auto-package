//! The refresh of a checkout, with a fake fetcher and a checkout made in a temp
//! dir: no network. The dependency is the real pixman archive, whose hash
//! `build.zig.zon` declares.

use super::fetch::{Fetch, Refusal, Response};
use super::refresh::refresh_checkout;
use ap_core::sink::NullReporter;
use std::cell::RefCell;
use std::io::Read;
use std::path::Path;

const PIXMAN: &[u8] = include_bytes!("testdata/pixman-0.3.0.tar.gz");
const PIXMAN_HASH: &str = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX";
const WAYLAND_HASH: &str = "wayland-0.6.0-lQa1kqz8AQADQmdNJsNhLoNHcnEGEUjrOaPV-dtEnEmX";
const PIXMAN_URL: &str = "https://codeberg.org/ifreund/zig-pixman/archive/v0.3.0.tar.gz";
/// The old vendor archive, which a refresh replaces.
const OLD: &[u8] = b"not a tarball at all";

/// Serves the pixman archive at its URL and records each request.
struct Fake {
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn new() -> Self {
        Fake {
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Fetch for Fake {
    fn get(&self, url: &str) -> Result<Response, Refusal> {
        self.calls.borrow_mut().push(url.to_string());
        if url == PIXMAN_URL {
            Ok(Response::Body {
                content_type: Some("application/gzip".to_string()),
                body: Box::new(std::io::Cursor::new(PIXMAN.to_vec())),
            })
        } else {
            Err(format!("no route for {url}").into())
        }
    }
}

fn tar_gz(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, content) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, format!("{top}/{path}"), *content)
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap()
}

/// The new Source0: a package whose manifest depends on pixman, declared with
/// `hash`.
fn source0(hash: &str, url: &str) -> Vec<u8> {
    let zon = format!(
        ".{{ .name = .pkg, .version = \"1.0.0\", .fingerprint = 0x3610a686c4bba334, \
         .dependencies = .{{ .pixman = .{{ .url = \"{url}\", .hash = \"{hash}\" }}, }}, }}"
    );
    tar_gz(
        "pkg-1.0",
        &[("build.zig.zon", zon.as_bytes()), ("src/main.zig", b"")],
    )
}

/// A checkout of a package whose spec is `spec`: the new Source0 and the old
/// vendor archive.
fn checkout(spec: &str, src0: &[u8], vendor_name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pkg.spec"), spec).unwrap();
    std::fs::write(dir.path().join("pkg-1.0.0.tar.gz"), src0).unwrap();
    std::fs::write(dir.path().join(vendor_name), OLD).unwrap();
    dir
}

/// A spec that refreshes `vendor_name`, reading the cache through `build_opts`.
fn spec(requires: &str, vendor_name: &str, build_opts: &str) -> String {
    format!(
        "Name:           pkg\n\
         Version:        1.0.0\n\
         Release:        0\n\
         Summary:        test\n\
         License:        MIT\n\
         Source0:        https://example.org/%{{name}}-%{{version}}.tar.gz\n\
         Source2:        {vendor_name}\n\
         BuildRequires:  {requires}\n\
         \n\
         %description\n\
         test\n\
         \n\
         %prep\n\
         %autosetup -n %{{name}}-%{{version}} -a2\n\
         \n\
         %build\n\
         %zig_build {build_opts}\n\
         \n\
         %check\n\
         %zig_test\n\
         \n\
         %install\n\
         %zig_install\n"
    )
}

/// The entries of a `vendor.tar.zst`, `.tar.gz` or `.tar.xz` file, by path.
fn entries(path: &Path) -> Vec<(String, bool)> {
    let bytes = std::fs::read(path).unwrap();
    let reader: Box<dyn Read> = match path.extension().and_then(|e| e.to_str()) {
        Some("zst") => Box::new(zstd::stream::read::Decoder::new(bytes.as_slice()).unwrap()),
        Some("xz") => Box::new(lzma_rust2::XzReader::new(bytes.as_slice(), false)),
        _ => Box::new(flate2::read::GzDecoder::new(bytes.as_slice())),
    };
    let mut ar = tar::Archive::new(reader);
    ar.entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.path().unwrap().to_string_lossy().into_owned(),
                e.header().entry_type().is_dir(),
            )
        })
        .collect()
}

#[test]
fn the_vendor_archive_is_rebuilt_from_build_zig_zon() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let fake = Fake::new();
    let work = tempfile::tempdir().unwrap();

    let got = refresh_checkout(dir.path(), &spec, "pkg", &fake, &NullReporter, work.path())
        .unwrap()
        .expect("the spec is refreshed");
    assert_eq!(got.file, "vendor.tar.zst");
    assert!(got.changed);
    assert_eq!(got.deps.len(), 1);
    assert_eq!(got.deps[0].hash, PIXMAN_HASH);
    assert_eq!(fake.calls.borrow().as_slice(), [PIXMAN_URL]);

    // zig 0.16 layout: one `.tar.gz` per dependency, nothing unpacked, no
    // build output.
    let names = entries(&dir.path().join("vendor.tar.zst"));
    assert!(
        names
            .iter()
            .any(|(p, _)| p == &format!("vendor/p/{PIXMAN_HASH}.tar.gz"))
    );
    assert!(
        names
            .iter()
            .all(|(p, _)| p == "vendor" || p.starts_with("vendor/"))
    );
    assert!(
        !names
            .iter()
            .any(|(p, _)| p.starts_with("vendor/b") || p.starts_with("vendor/o"))
    );
    assert!(
        !names
            .iter()
            .any(|(p, d)| *d && p == &format!("vendor/p/{PIXMAN_HASH}"))
    );
}

#[test]
fn a_second_refresh_of_the_same_source_changes_nothing() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap();
    let first = std::fs::read(dir.path().join("vendor.tar.zst")).unwrap();

    let work = tempfile::tempdir().unwrap();
    let got = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .unwrap();
    assert!(!got.changed);
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        first
    );
}

#[test]
fn zig_015_gets_unpacked_directories_in_the_same_archive_name() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec("zig0.15", "vendor.tar.zst", "--global-cache-dir vendor/");
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .unwrap();
    let names = entries(&dir.path().join("vendor.tar.zst"));
    assert!(
        names
            .iter()
            .any(|(p, d)| *d && p == &format!("vendor/p/{PIXMAN_HASH}"))
    );
    assert!(!names.iter().any(|(p, _)| p.ends_with(".tar.gz")));
}

#[test]
fn a_gzip_archive_stays_gzip() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.gz",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.gz");
    let work = tempfile::tempdir().unwrap();
    let got = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got.file, "vendor.tar.gz");
    let names = entries(&dir.path().join("vendor.tar.gz"));
    assert!(
        names
            .iter()
            .any(|(p, _)| p == &format!("vendor/p/{PIXMAN_HASH}.tar.gz"))
    );
}

#[test]
fn a_system_cache_is_written_under_the_path_the_build_names() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--system %{_builddir}/%{name}-%{version}/vendor/zig/p",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .unwrap();
    let names = entries(&dir.path().join("vendor.tar.zst"));
    assert!(
        names
            .iter()
            .any(|(p, _)| p == &format!("vendor/zig/p/{PIXMAN_HASH}.tar.gz"))
    );
}

#[test]
fn a_spec_that_reads_no_cache_in_build_is_left_alone() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    // The flag is in %check only, so the build does not read the archive.
    let spec = spec("zig >= 0.16.0", "vendor.tar.zst", "")
        .replace("%zig_test\n", "%zig_test --global-cache-dir vendor/\n");
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let fake = Fake::new();
    let work = tempfile::tempdir().unwrap();
    assert!(
        refresh_checkout(dir.path(), &spec, "pkg", &fake, &NullReporter, work.path())
            .unwrap()
            .is_none()
    );
    assert!(fake.calls.borrow().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        OLD
    );
}

#[test]
fn a_spec_without_zig_or_without_a_vendor_source_is_left_alone() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let no_zig = spec("cmake", "vendor.tar.zst", "--global-cache-dir vendor/");
    let dir = checkout(&no_zig, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    assert!(
        refresh_checkout(
            dir.path(),
            &no_zig,
            "pkg",
            &Fake::new(),
            &NullReporter,
            work.path()
        )
        .unwrap()
        .is_none()
    );

    let no_vendor = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/",
    )
    .replace(
        "Source2:        vendor.tar.zst",
        "Source2:        https://example.org/x.tar.gz",
    );
    let work = tempfile::tempdir().unwrap();
    assert!(
        refresh_checkout(
            dir.path(),
            &no_vendor,
            "pkg",
            &Fake::new(),
            &NullReporter,
            work.path()
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn a_zig_requirement_without_a_version_is_refused_before_any_request() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec("zig", "vendor.tar.zst", "--global-cache-dir vendor/");
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let fake = Fake::new();
    let work = tempfile::tempdir().unwrap();
    let err = refresh_checkout(dir.path(), &spec, "pkg", &fake, &NullReporter, work.path())
        .unwrap_err()
        .msg;
    assert!(err.contains("version"), "{err}");
    assert!(fake.calls.borrow().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        OLD
    );
}

#[test]
fn an_xz_vendor_archive_stays_xz_and_is_read_back_as_xz() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.xz",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.xz");
    let work = tempfile::tempdir().unwrap();
    let got = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got.file, "vendor.tar.xz");
    assert!(got.changed);
    let names = entries(&dir.path().join("vendor.tar.xz"));
    assert!(
        names
            .iter()
            .any(|(p, _)| p == &format!("vendor/p/{PIXMAN_HASH}.tar.gz"))
    );
}

#[test]
fn a_second_refresh_of_an_xz_source_writes_the_same_bytes() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.xz",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.xz");
    let work = tempfile::tempdir().unwrap();
    refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap();
    let first = std::fs::read(dir.path().join("vendor.tar.xz")).unwrap();
    let work = tempfile::tempdir().unwrap();
    let got = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .unwrap();
    assert!(!got.changed);
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.xz")).unwrap(),
        first
    );
}

#[test]
fn a_hash_mismatch_ends_the_refresh_and_writes_nothing() {
    let src0 = source0(WAYLAND_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    let err = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap_err()
    .msg;
    assert!(err.contains("archive hash"), "{err}");
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        OLD
    );
}

#[test]
fn a_dependency_off_the_allowlist_is_refused_before_it_is_requested() {
    let url = "https://evil.example.net/pixman-0.3.0.tar.gz";
    let src0 = source0(PIXMAN_HASH, url);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let fake = Fake::new();
    let work = tempfile::tempdir().unwrap();
    let err = refresh_checkout(dir.path(), &spec, "pkg", &fake, &NullReporter, work.path())
        .unwrap_err()
        .msg;
    assert!(err.contains("evil.example.net"), "{err}");
    assert!(fake.calls.borrow().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        OLD
    );
}

#[test]
fn a_source0_without_build_zig_zon_is_refused() {
    let src0 = tar_gz("pkg-1.0", &[("src/main.zig", b"")]);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    let err = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap_err()
    .msg;
    assert!(err.contains("build.zig.zon"), "{err}");
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        OLD
    );
}

#[test]
fn a_build_that_reads_two_caches_is_refused() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--global-cache-dir vendor/ --system %{_builddir}/%{name}-%{version}/other/p",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    let err = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap_err()
    .msg;
    assert!(err.contains("two zig caches"), "{err}");
}

#[test]
fn a_cache_flag_in_a_macro_the_build_uses_counts_as_the_build_reading_it() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    // The waylock form: the flag lives in a %global that %build expands.
    let spec = spec("zig >= 0.16.0", "vendor.tar.zst", "%{zig_opts}").replace(
        "Source0:",
        "%global zig_opts --global-cache-dir vendor/ -Dpie\nSource0:",
    );
    let dir = checkout(&spec, &src0, "vendor.tar.zst");
    let work = tempfile::tempdir().unwrap();
    let got = refresh_checkout(
        dir.path(),
        &spec,
        "pkg",
        &Fake::new(),
        &NullReporter,
        work.path(),
    )
    .unwrap()
    .expect("the macro's cache is the build's cache");
    assert!(got.changed);
    let names = entries(&dir.path().join("vendor.tar.zst"));
    assert!(
        names
            .iter()
            .any(|(p, _)| p == &format!("vendor/p/{PIXMAN_HASH}.tar.gz"))
    );
}
