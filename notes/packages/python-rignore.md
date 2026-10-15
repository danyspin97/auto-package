# python-rignore

## Build requirements
- The package is a maturin project with a Rust extension, so the build needs cargo and PyO3 available offline. The maturin BuildRequires appears once; a duplicate was removed.
- The pyproject build requirement pins maturin to at least 1.14.1 and below 2.0. The upper bound must stay outside the %{python_module} macro: inside it the `< 2.0` is not parsed as intended.
- git-core is a BuildRequires because the walk tests in the test suite run the git executable. Without it they fail with FileNotFoundError for 'git'.
- Because the package builds a compiled extension, it is not noarch. The spec uses arch-specific sitearch paths, following the maturin shape for Rust extensions.
Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Cargo crates come from a registry tarball shipped as Source1, because the build has no network access. The cargo and PyO3 environment is set in %build.
- PYO3_PYTHON is exported per Python flavor inside the same expansion as the wheel build. A nested %python_expand was flattened so both share one expansion.
Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Four walk tests failed with 'git' not found. Fixed by adding git-core to BuildRequires.
Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.
