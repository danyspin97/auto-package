
use super::*;

fn pkgver(spec: &str) -> Option<String> {
    collect_simple_defines(spec).get("pkgver").cloned()
}

#[test]
fn define_follows_the_evaluable_branch() {
    let tmpl = |flag: &str| {
        format!(
            "%define flag {flag}\n%if 0%{{?flag}}\n%define pkgver 2.0\n%else\n%define pkgver 1.0\n%endif\n"
        )
    };
    assert_eq!(pkgver(&tmpl("1")).as_deref(), Some("2.0"));
    assert_eq!(pkgver(&tmpl("0")).as_deref(), Some("1.0"));
}

#[test]
fn define_in_unevaluable_branch_is_not_recorded() {
    let spec = "%if 0%{?suse_version}\n%define pkgver 2.0\n%else\n%define pkgver 1.0\n%endif\n";
    assert_eq!(pkgver(spec), None);
    let spec = "%ifarch x86_64\n%define pkgver 2.0\n%endif\n";
    assert_eq!(pkgver(spec), None);
    assert_eq!(
        pkgver("%if 1\n%define pkgver 3.0\n%endif\n").as_deref(),
        Some("3.0")
    );
}

#[test]
fn autosetup_fix_only_replaces_whole_version_component() {
    let spec = "Name: sqlite3\nVersion: 3\n%autosetup -n wrong\n";
    let out = fix_autosetup_dir(spec, "sqlite3-3").unwrap();
    assert!(out.contains("-n sqlite3-%{version}"), "{out}");
    let spec = "Name: markupsafe\nVersion: 3.0.3\n%autosetup -n wrong\n";
    let out = fix_autosetup_dir(spec, "MarkupSafe-3.0.3").unwrap();
    assert!(out.contains("-n MarkupSafe-%{version}"), "{out}");
}

/// A hostile top-level name must never reach the spec text: no line is
/// injected and the spec is left alone.
#[test]
fn autosetup_fix_refuses_unsafe_top_level_names() {
    let spec = "Name: thing\nVersion: 1.0\n%autosetup -n thing-1.0\n";
    for bad in [
        "thing-1.0\n%post\nrm -rf /",
        "thing-%{version}",
        "thing 1.0",
        "thing-1.0;reboot",
        "thing/../x",
        "",
    ] {
        assert_eq!(fix_autosetup_dir(spec, bad), None, "accepted {bad:?}");
    }
    assert!(is_safe_autosetup_dir("thing-1.0"));
    assert!(is_safe_autosetup_dir("MarkupSafe-3.0.3"));
    assert!(is_safe_autosetup_dir("Pyöpt-1.0"));
    for bad in [
        "a b", "a\nb", "a%b", "a/b", "a\\b", "a'b", "a;b", "a$b", "a`b", "a|b", "a&b", "a\u{7}b",
    ] {
        assert!(!is_safe_autosetup_dir(bad), "{bad:?}");
    }
}

#[test]
fn dist_info_rewrite_keeps_the_trailing_file() {
    let out = normalize_files_paths(
        "%files\n%license %{python_sitelib}/foo-%{version}.dist-info/LICENSE\n",
        "foo",
    );
    assert_eq!(
        out,
        "%files\n%license %{python_sitelib}/foo-%{version}.dist-info/LICENSE\n"
    );
}

#[test]
fn non_spdx_classifiers_do_not_produce_an_empty_or_osi_name() {
    let l = |c: &str| license_tag(&License::Classifiers(vec![c.to_string()]));
    assert_eq!(l("License :: Other/Proprietary License"), "FIXME");
    assert_eq!(l("License :: OSI Approved :: MIT License"), "MIT");
}

#[test]
fn build_requires_lists_names_without_bounds_or_duplicates() {
    use BuildRequire::*;
    let spec = "Name: foo\n\
                    BuildRequires:  cmake >= 3.10\n\
                    BuildRequires:  gcc-c++, libfoo-devel<2 libbar-devel\n\
                    BuildRequires:  cmake\n\
                    BuildRequires(pre):  fdupes\n\
                    Requires:       notthis\n\
                    BuildRequires:  libx >=1.0\n";
    assert_eq!(
        build_requires(spec),
        [
            Package("cmake".into()),
            Package("gcc-c++".into()),
            Package("libfoo-devel".into()),
            Package("libbar-devel".into()),
            Package("fdupes".into()),
            Package("libx".into()),
        ]
    );
}

#[test]
fn build_requires_keeps_capabilities_whole() {
    use BuildRequire::*;
    let spec = "BuildRequires:  pkgconfig(glib-2.0) >= 2.5 %{python_module foo >= 1} /usr/bin/x\n";
    assert_eq!(
        build_requires(spec),
        [
            Capability("pkgconfig(glib-2.0)".into()),
            Capability("%{python_module foo >= 1}".into()),
            Capability("/usr/bin/x".into()),
        ]
    );
}
