use super::build_switch::*;

const AUTOTOOLS: &str = "Name: foo\nVersion: 1.0\nBuildRequires: autoconf\nBuildRequires: automake\n\
%build\n%configure\n%make_build\n%install\n%make_install\n";

const MESON: &str = "Name: foo\nVersion: 2.0\nBuildRequires: meson >= 1.0\nBuildRequires: ninja\n\
%build\n%meson\n%meson_build\n%install\n%meson_install\n";

const CMAKE: &str = "Name: foo\nVersion: 2.0\nBuildRequires: cmake\n\
%build\n%cmake\n%cmake_build\n%install\n%cmake_install\n";

#[test]
fn the_macros_name_the_build_system() {
    assert_eq!(build_system(AUTOTOOLS), Some(BuildSystem::Autotools));
    assert_eq!(build_system(MESON), Some(BuildSystem::Meson));
    assert_eq!(build_system(CMAKE), Some(BuildSystem::CMake));
}

#[test]
fn buildrequires_name_the_system_only_when_no_macro_does() {
    let requires_only = "Name: foo\nBuildRequires: meson >= 1.0\n%build\n%{nothing}\n";
    assert_eq!(build_system(requires_only), Some(BuildSystem::Meson));
    let both = "Name: foo\nBuildRequires: meson\n%build\n%configure\n%make_build\n";
    assert_eq!(build_system(both), Some(BuildSystem::Autotools));
}

#[test]
fn two_systems_or_no_system_is_no_system() {
    let mixed = "Name: foo\n%build\n%configure\n%cmake\n";
    assert_eq!(build_system(mixed), None);
    assert_eq!(build_system("Name: foo\n%build\nmake\n"), None);
}

#[test]
fn a_commented_macro_does_not_name_a_system() {
    let spec = "Name: foo\n# %meson was the old way\n%build\n%configure\n";
    assert_eq!(build_system(spec), Some(BuildSystem::Autotools));
}

#[test]
fn a_switch_is_a_change_of_system_between_two_specs() {
    assert_eq!(
        switch_between(AUTOTOOLS, MESON),
        Some(Switch {
            from: BuildSystem::Autotools,
            to: BuildSystem::Meson
        })
    );
    assert_eq!(switch_between(MESON, MESON), None);
    assert_eq!(switch_between("Name: foo\n", MESON), None);
}

#[test]
fn the_bullet_names_both_systems() {
    let s = switch_between(AUTOTOOLS, MESON).unwrap();
    assert_eq!(
        s.bullet(),
        "Switch to the meson build system: upstream removed autotools"
    );
    assert_eq!(
        with_switch_bullet("Update to 2.0".into(), Some(s)),
        "Update to 2.0\nSwitch to the meson build system: upstream removed autotools"
    );
    assert_eq!(
        with_switch_bullet("Update to 2.0".into(), None),
        "Update to 2.0"
    );
}

#[test]
fn evidence_must_name_the_old_system_and_say_it_is_removed() {
    let s = switch_between(AUTOTOOLS, MESON).unwrap();
    let removed = vec!["NEWS: the Autotools build system has been removed".to_string()];
    assert!(evidence_shows_removal(s, &removed, ""));
    assert!(evidence_shows_removal(
        s,
        &[],
        "configure.ac is gone from the tarball."
    ));
    // Naming the old system without saying it went away is not evidence.
    let uses = vec!["the spec still calls %configure".to_string()];
    assert!(!evidence_shows_removal(s, &uses, "autotools is still used"));
    // Saying something went away without naming the old system is not evidence.
    assert!(!evidence_shows_removal(
        s,
        &["the tarball lost a file".to_string()],
        ""
    ));
}
