use super::wait_bounds::*;

/// python-pyinstaller after the translated turn: the runtime and the test
/// BuildRequires both ask `pyinstaller-hooks-contrib >= 2026.7`.
#[test]
fn a_spec_bound_on_a_waited_name_is_read_in_any_flavour() {
    let spec = "Name: python-PyInstaller\n\
                Requires:       python313-pyinstaller-hooks-contrib >= 2026.7\n\
                BuildRequires:  python-pyinstaller-hooks-contrib >= 2026.7\n\
                Requires:       python313-altgraph >= 0.17\n";
    let deps = vec!["pyinstaller-hooks-contrib".to_string()];
    assert_eq!(
        spec_bounds(spec, &deps),
        [(
            "pyinstaller-hooks-contrib".to_string(),
            "2026.7".to_string()
        )]
    );
}

/// A name the package does not wait on, a name without a bound, and a name
/// that only shares a prefix with a waited one, are not bounds on it.
#[test]
fn only_the_waited_name_with_a_bound_counts() {
    let spec = "Requires: python313-altgraph >= 0.17\n\
                Requires: python313-pyinstaller-hooks-contrib\n\
                Requires: python313-pyinstaller-hooks-contrib-extra >= 9.0\n";
    let deps = vec!["pyinstaller-hooks-contrib".to_string()];
    assert!(spec_bounds(spec, &deps).is_empty());
}
