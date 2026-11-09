use super::*;

/// A source tree under `root` with the given files (relative path, contents).
fn tree(root: &std::path::Path, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

#[test]
fn a_test_that_imports_pytest_needs_it() {
    let dir = tempfile::tempdir().unwrap();
    tree(
        dir.path(),
        &[
            ("foo-1.0/setup.py", "import pytest\n"),
            ("foo-1.0/tests/test_core.py", "import unittest\n"),
            ("foo-1.0/tests/test_more.py", "from pytest import raises\n"),
        ],
    );
    assert!(tests_may_need_pytest(dir.path()));
}

#[test]
fn a_test_that_uses_the_pytest_api_needs_it() {
    let dir = tempfile::tempdir().unwrap();
    tree(
        dir.path(),
        &[(
            "tests/test_core.py",
            "@pytest.mark.slow\ndef test_a():\n    pass\n",
        )],
    );
    assert!(tests_may_need_pytest(dir.path()));
}

#[test]
fn a_tree_whose_tests_never_import_pytest_does_not_need_it() {
    let dir = tempfile::tempdir().unwrap();
    tree(
        dir.path(),
        &[
            (
                "foo-1.0/tests/test_core.py",
                "import unittest\n\nclass T(unittest.TestCase):\n    pass\n",
            ),
            // Not a test file: its import does not count.
            ("foo-1.0/foo/helpers.py", "import pytest\n"),
            ("foo-1.0/tests/data.txt", "import pytest\n"),
        ],
    );
    assert!(!tests_may_need_pytest(dir.path()));
}

#[test]
fn a_missing_source_tree_is_taken_as_needing_it() {
    let dir = tempfile::tempdir().unwrap();
    assert!(tests_may_need_pytest(&dir.path().join("absent")));
}

#[test]
fn a_pyunittest_check_keeps_the_line_when_the_tests_import_pytest() {
    let spec = "BuildRequires:  %{python_module pytest}\n%check\n%pyunittest -v\n\n%files\n";
    assert_eq!(drop_unused_pytest_build_requires(spec, true), spec);
    let out = drop_unused_pytest_build_requires(spec, false);
    assert!(!out.contains("module pytest}"), "{out}");
}

#[test]
fn only_the_pytest_tag_is_recognised() {
    assert!(is_pytest_build_requires(
        "BuildRequires:  %{python_module pytest}"
    ));
    assert!(!is_pytest_build_requires(
        "BuildRequires:  %{python_module pytest-xdist}"
    ));
    assert!(!is_pytest_build_requires(
        "# BuildRequires:  %{python_module pytest}"
    ));
}
