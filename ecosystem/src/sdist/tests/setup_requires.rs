use super::*;

/// The `setup_requires` of a setup.cfg `[options]` section, read as text.
#[test]
fn setup_cfg_lists_one_entry_per_line() {
    let text = "[metadata]\nname = thing\n\n[options]\nsetup_requires =\n    setuptools>=40.8\n    wheel\ninstall_requires =\n    requests\n";
    assert_eq!(
        setup_cfg_setup_requires(text),
        ["setuptools>=40.8", "wheel"]
    );
}

#[test]
fn setup_cfg_accepts_comma_separated_entries_on_one_line() {
    let text = "[options]\nsetup_requires = setuptools>=42, wheel\n";
    assert_eq!(setup_cfg_setup_requires(text), ["setuptools>=42", "wheel"]);
}

/// setuptools splits a multi-line value by line only, so a comma inside a
/// specifier stays part of that entry.
#[test]
fn setup_cfg_multi_line_keeps_a_comma_inside_a_specifier() {
    let text = "[options]\nsetup_requires =\n    setuptools>=40,<60\n    wheel\n";
    assert_eq!(
        setup_cfg_setup_requires(text),
        ["setuptools>=40,<60", "wheel"]
    );
}

#[test]
fn setup_cfg_without_setup_requires_gives_nothing() {
    let text = "[options]\ninstall_requires =\n    requests>=2\n";
    assert!(setup_cfg_setup_requires(text).is_empty());
}

#[test]
fn setup_cfg_file_directive_is_not_read() {
    let text = "[options]\nsetup_requires = file: build-requirements.txt\n";
    assert!(setup_cfg_setup_requires(text).is_empty());
}

#[test]
fn setup_cfg_keys_outside_options_are_not_read() {
    let text = "[metadata]\nsetup_requires = wheel\n";
    assert!(setup_cfg_setup_requires(text).is_empty());
}

/// Without a pyproject `[build-system]`, the setup.cfg entry replaces the
/// default `setuptools` for the same project and adds the rest.
#[test]
fn setup_cfg_at_root_replaces_the_default_setuptools() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[(
            "thing-1.0/setup.cfg",
            b"[options]\nsetup_requires = Setuptools>=40.8, wheel\n".as_slice(),
        )],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.build_system.requires, ["Setuptools>=40.8", "wheel"]);
}

/// The pyproject `[build-system]` entry for a project wins over the setup.cfg
/// one; a project it does not name is added.
#[test]
fn setup_cfg_duplicate_of_a_pyproject_requirement_keeps_the_pyproject_entry() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[build-system]\nrequires = [\"setuptools>=61\", \"wheel\"]\n".as_slice(),
            ),
            (
                "thing-1.0/setup.cfg",
                b"[options]\nsetup_requires =\n    setuptools>=40\n    cython\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(
        info.build_system.requires,
        ["setuptools>=61", "wheel", "cython"]
    );
}

/// The setup.cfg of a sub-directory is not read when the chosen pyproject
/// sits at the root: it belongs to another project.
#[test]
fn setup_cfg_in_a_subdirectory_is_not_read_for_a_root_pyproject() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[build-system]\nrequires = [\"hatchling\"]\n".as_slice(),
            ),
            (
                "thing-1.0/sub/setup.cfg",
                b"[options]\nsetup_requires = cython\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.build_system.requires, ["hatchling"]);
}

/// install_requires is runtime, not build, and is never read here.
#[test]
fn setup_cfg_install_requires_is_not_a_build_requirement() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[(
            "thing-1.0/setup.cfg",
            b"[options]\ninstall_requires =\n    requests>=2\n".as_slice(),
        )],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.build_system.requires, ["setuptools"]);
}
