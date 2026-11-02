//! The build's cache flags as the vendor step reads them: which paths name a
//! cache, and what a `%{buildsubdir}` stands for.

use super::plan::cache_roots;

const PLAIN: &str = "Name: x\nVersion: 1.0\n%autosetup\n";

#[test]
fn a_global_cache_dir_the_shell_would_expand_is_refused() {
    for arg in ["$PWD/vendor", "\"vendor\"", "'vendor'", "`pwd`/vendor"] {
        let build = format!("zig build --global-cache-dir {arg}");
        assert!(cache_roots(PLAIN, &build).is_err(), "{arg}");
    }
}

#[test]
fn a_global_cache_dir_with_an_undefined_macro_is_refused() {
    let build = "zig build --global-cache-dir %{_builddir}/vendor";
    assert!(cache_roots(PLAIN, build).is_err());
}

#[test]
fn a_leading_dot_slash_is_dropped_from_a_global_cache_dir() {
    let build = "zig build --global-cache-dir ./vendor/";
    assert_eq!(cache_roots(PLAIN, build).unwrap(), ["vendor"]);
}

#[test]
fn a_system_path_expands_buildsubdir_to_the_autosetup_directory() {
    let spec = "Name: x\nVersion: 1.0\n%autosetup -n foo-1.0\n";
    let build = "zig build --system %{_builddir}/%{buildsubdir}/vendor/p";
    assert_eq!(cache_roots(spec, build).unwrap(), ["vendor"]);
}

#[test]
fn a_buildsubdir_macro_names_the_build_directory() {
    let spec = "%global buildsubdir bar\n%autosetup -n foo-1.0\n";
    let build = "zig build --system %{_builddir}/bar/vendor/p";
    assert_eq!(cache_roots(spec, build).unwrap(), ["vendor"]);
}
