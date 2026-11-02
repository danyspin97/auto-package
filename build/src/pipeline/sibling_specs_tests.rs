use super::sibling_specs::{SiblingBump, sibling_spec_sources};
use ap_core::sink::NullReporter;

/// Bump the siblings of `main` from `old` to `new`, as the update does.
fn bump(dir: &std::path::Path, main: &str, old: &str, new: &str) -> SiblingBump {
    let step = ap_core::event::StepId(0);
    SiblingBump::apply(dir, &dir.join(main), old, new, "pkg", &NullReporter, step)
}

/// dbus-1-devel-doc.spec as the package has it at 1.14.10 (trimmed).
const DBUS_DOC: &str = "Name:           dbus-1-devel-doc
Version:        1.14.10
Release:        0
Summary:        Developer documentation for D-Bus
License:        GFDL-1.3-or-later AND AFL-2.1 OR GPL-2.0-or-later
%define _name   dbus
Source0:        https://dbus.freedesktop.org/releases/dbus/%{_name}-%{version}.tar.xz
Source1:        https://dbus.freedesktop.org/releases/dbus/%{_name}-%{version}.tar.xz.asc
Source2:        dbus-1.keyring
%description
Developer documentation for D-Bus.
%files
%doc %{_datadir}/doc/dbus/dbus-faq.html
";

/// elfutils-debuginfod.spec as the package has it at 0.196 (trimmed).
const ELFUTILS_DEBUGINFOD: &str = "Name:           elfutils-debuginfod
Version:        0.196
Release:        0
Summary:        Elf-utils debuginfod server
License:        GPL-3.0-or-later
Source:         https://sourceware.org/elfutils/ftp/%{version}/elfutils-%{version}.tar.bz2
Source1:        https://sourceware.org/elfutils/ftp/%{version}/elfutils-%{version}.tar.bz2.sig
Source3:        elfutils.keyring
%description
Debuginfod server.
%files
%doc AUTHORS NEWS README
";

fn write_package(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in files {
        std::fs::write(dir.path().join(name), body).unwrap();
    }
    dir
}

#[test]
fn a_sibling_spec_at_the_old_version_moves_with_the_main_spec() {
    let main = "Name: dbus-1\nVersion: 1.14.10\nSource0: x.tar.xz\n";
    let dir = write_package(&[("dbus-1.spec", main), ("dbus-1-devel-doc.spec", DBUS_DOC)]);
    let main_path = dir.path().join("dbus-1.spec");
    bump(dir.path(), "dbus-1.spec", "1.14.10", "1.16.2").keep();
    let text = std::fs::read_to_string(dir.path().join("dbus-1-devel-doc.spec")).unwrap();
    assert!(text.contains("Version:        1.16.2"), "{text}");
    // The archive the bumped sibling names is the new one.
    let names: Vec<String> = sibling_spec_sources(dir.path(), &main_path)
        .into_iter()
        .map(|s| s.file)
        .collect();
    assert!(
        names.contains(&"dbus-1.16.2.tar.xz".to_string()),
        "{names:?}"
    );
    assert!(!names.iter().any(|n| n.contains("1.14.10")), "{names:?}");
}

#[test]
fn a_sibling_at_another_version_is_left_and_reported() {
    let dir = write_package(&[
        ("elfutils.spec", "Name: elfutils\nVersion: 0.197\n"),
        (
            "elfutils-debuginfod.spec",
            &ELFUTILS_DEBUGINFOD.replace("0.196", "0.195"),
        ),
    ]);
    bump(dir.path(), "elfutils.spec", "0.196", "0.197").keep();
    let text = std::fs::read_to_string(dir.path().join("elfutils-debuginfod.spec")).unwrap();
    assert!(text.contains("Version:        0.195"));
}

#[test]
fn elfutils_debuginfod_moves_to_the_new_archive() {
    let dir = write_package(&[
        ("elfutils.spec", "Name: elfutils\nVersion: 0.197\n"),
        ("elfutils-debuginfod.spec", ELFUTILS_DEBUGINFOD),
    ]);
    bump(dir.path(), "elfutils.spec", "0.196", "0.197").keep();
    let main_path = dir.path().join("elfutils.spec");
    let names: Vec<String> = sibling_spec_sources(dir.path(), &main_path)
        .into_iter()
        .map(|s| s.file)
        .collect();
    assert!(
        names.contains(&"elfutils-0.197.tar.bz2".to_string()),
        "{names:?}"
    );
}

#[test]
fn a_failed_update_puts_the_siblings_back() {
    let dir = write_package(&[
        ("elfutils.spec", "Name: elfutils\nVersion: 0.197\n"),
        ("elfutils-debuginfod.spec", ELFUTILS_DEBUGINFOD),
    ]);
    let before = std::fs::read_to_string(dir.path().join("elfutils-debuginfod.spec")).unwrap();
    drop(bump(dir.path(), "elfutils.spec", "0.196", "0.197"));
    let after = std::fs::read_to_string(dir.path().join("elfutils-debuginfod.spec")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn a_literal_version_on_the_unpack_line_follows_the_bump() {
    let sibling =
        "Name: foo-doc\nVersion: 1.0\nSource: foo-%{version}.tar.gz\n%prep\n%setup -q -n foo-1.0\n";
    let dir = write_package(&[
        ("foo.spec", "Name: foo\nVersion: 1.1\n"),
        ("foo-doc.spec", sibling),
    ]);
    bump(dir.path(), "foo.spec", "1.0", "1.1").keep();
    let text = std::fs::read_to_string(dir.path().join("foo-doc.spec")).unwrap();
    assert!(text.contains("%setup -q -n foo-1.1"), "{text}");
}
