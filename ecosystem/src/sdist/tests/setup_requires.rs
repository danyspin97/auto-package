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

/// A literal list of plain strings in setup.py is read as text, not run.
#[test]
fn setup_py_literal_list_is_read() {
    let text = "from setuptools import setup\nsetup(\n    name=\"thing\",\n    setup_requires=[\"setuptools>=40.8\", 'wheel'],\n)\n";
    assert_eq!(setup_py_setup_requires(text), ["setuptools>=40.8", "wheel"]);
}

#[test]
fn setup_py_empty_list_gives_nothing() {
    assert!(setup_py_setup_requires("setup(setup_requires=[])\n").is_empty());
}

/// A name, a call or a concatenation has a value only a run would know.
#[test]
fn setup_py_non_literal_list_gives_nothing() {
    for text in [
        "setup(setup_requires=REQUIRES)\n",
        "setup(setup_requires=get_requires())\n",
        "setup(setup_requires=['a'] + extra)\n",
        "setup(setup_requires=[f'{name}>=1'])\n",
        "setup(setup_requires=['a',  # comment\n 'b'])\n",
        "setup(setup_requires=['a', name])\n",
    ] {
        assert!(setup_py_setup_requires(text).is_empty(), "{text}");
    }
}

/// A second assignment makes the value ambiguous, so none of them is read.
#[test]
fn setup_py_two_assignments_give_nothing() {
    let text = "setup_requires = ['a']\nsetup(setup_requires=['b'])\n";
    assert!(setup_py_setup_requires(text).is_empty());
}

#[test]
fn setup_py_comparison_or_longer_name_is_not_an_assignment() {
    let text = "x = setup_requires == ['a']\nmy_setup_requires = ['b']\n";
    assert!(setup_py_setup_requires(text).is_empty());
}

/// A commented-out assignment is not an assignment: the live one is read.
#[test]
fn setup_py_commented_assignment_is_not_read() {
    let text = "# setup_requires = ['old']\nsetup(setup_requires=['wheel'])\n";
    assert_eq!(setup_py_setup_requires(text), ["wheel"]);
    let text =
        "setup(\n    name='thing',  # setup_requires = ['old']\n    setup_requires=['wheel'],\n)\n";
    assert_eq!(setup_py_setup_requires(text), ["wheel"]);
}

/// An attribute such as `self.setup_requires = [...]` is not the keyword
/// argument, so it does not count as a second assignment either.
#[test]
fn setup_py_attribute_assignment_is_not_read() {
    let text = "self.setup_requires = ['old']\nsetup(setup_requires=['wheel'])\n";
    assert_eq!(setup_py_setup_requires(text), ["wheel"]);
    assert!(setup_py_setup_requires("self.setup_requires = ['old']\n").is_empty());
}

/// Without a pyproject `[build-system]`, a literal setup.py list at the root
/// replaces the default `setuptools`, as a setup.cfg entry does.
#[test]
fn setup_py_at_root_replaces_the_default_setuptools() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[(
            "thing-1.0/setup.py",
            b"from setuptools import setup\nsetup(setup_requires=['setuptools>=40.8', 'wheel'])\n"
                .as_slice(),
        )],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.build_system.requires, ["setuptools>=40.8", "wheel"]);
}

/// A pyproject `[build-system]` entry for a project wins over the setup.py one.
#[test]
fn setup_py_duplicate_of_a_pyproject_requirement_keeps_the_pyproject_entry() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("src.tar.gz");
    write_tar(
        &tarball,
        &[
            (
                "thing-1.0/pyproject.toml",
                b"[build-system]\nrequires = [\"setuptools>=61\"]\n".as_slice(),
            ),
            (
                "thing-1.0/setup.py",
                b"setup(setup_requires=['Setuptools>=40', 'cython'])\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.build_system.requires, ["setuptools>=61", "cython"]);
}

/// A non-literal setup.py leaves the build requirements as the pyproject has them.
#[test]
fn setup_py_non_literal_leaves_the_pyproject_requirements() {
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
                "thing-1.0/setup.py",
                b"setup(setup_requires=REQS)\n".as_slice(),
            ),
        ],
    );
    let info = inspect(&tarball).unwrap();
    assert_eq!(info.build_system.requires, ["hatchling"]);
}
