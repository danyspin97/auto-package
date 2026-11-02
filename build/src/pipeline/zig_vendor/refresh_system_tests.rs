//! The refresh of a checkout whose build reads its cache through `--system`:
//! the path it names, the macros and build subdirectory it may use, and the
//! paths it refuses. Same fake fetcher and checkouts as `refresh_tests`.

use super::refresh::refresh_checkout;
use super::refresh_tests::{Fake, OLD, PIXMAN_HASH, PIXMAN_URL, checkout, entries, source0, spec};
use ap_core::sink::NullReporter;

/// The spec with `build_opts` on its zig call, and with `prelude` (macro
/// definitions) before its Source0.
fn spec_with(build_opts: &str, prelude: &str) -> String {
    spec("zig >= 0.16.0", "vendor.tar.zst", build_opts)
        .replace("Source0:", &format!("{prelude}Source0:"))
}

#[test]
fn a_system_cache_named_by_a_macro_is_written_under_its_expanded_path() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec_with(
        "--system %{zig_cache}",
        "%global zig_cache %{_builddir}/%{name}-%{version}/vendor/zig/p\n",
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
fn a_system_cache_follows_the_build_subdirectory_the_spec_names() {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec(
        "zig >= 0.16.0",
        "vendor.tar.zst",
        "--system %{_builddir}/src-dir/cache/p",
    )
    .replace("-n %{name}-%{version} -a2", "-n src-dir -a2");
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
            .any(|(p, _)| p == &format!("cache/p/{PIXMAN_HASH}.tar.gz"))
    );
}

/// A `--system` path the refresh refuses: the reason it names, and the old
/// archive is kept.
fn refused_system(build_opts: &str, prelude: &str) -> String {
    let src0 = source0(PIXMAN_HASH, PIXMAN_URL);
    let spec = spec_with(build_opts, prelude);
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
    assert_eq!(
        std::fs::read(dir.path().join("vendor.tar.zst")).unwrap(),
        OLD
    );
    err
}

#[test]
fn a_system_path_outside_the_build_directory_is_refused_with_the_reason() {
    let why = refused_system("--system /opt/zig/vendor/p", "");
    assert!(
        why.contains("absolute path outside the build directory"),
        "{why}"
    );
    let why = refused_system("--system %{_builddir}/other-1.0/vendor/p", "");
    assert!(why.contains("outside the build directory"), "{why}");
}

#[test]
fn a_system_path_with_a_macro_the_spec_does_not_define_is_refused() {
    let why = refused_system("--system %{_builddir}/%{name}-%{version}/%{vdir}/p", "");
    assert!(
        why.contains("%{vdir}") && why.contains("does not define"),
        "{why}"
    );
}

#[test]
fn a_system_path_that_climbs_out_of_the_build_subdirectory_is_refused() {
    let why = refused_system("--system %{_builddir}/%{name}-%{version}/../vendor/p", "");
    assert!(why.contains("not a plain relative directory"), "{why}");
}
