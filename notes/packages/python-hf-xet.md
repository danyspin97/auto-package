# python-hf-xet notes

## Build requirements
- The package is a compiled PyO3 extension built with maturin. It is not noarch; it is ExclusiveArch and installs into the sitearch path.
- Crates come from an offline Cargo registry shipped as Source1, because the build has no network access for crates. Unpacking it needs zstd. Building needs the Rust toolchain with cargo and a C++ compiler.
- maturin is the build backend. Declare it once, with its version constraint on its own line.
- ca-certificates and ca-certificates-mozilla are needed so the %check environment has a system CA bundle. Without one, the HTTP client in the tests panics.

Checked against 1.7.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- None deselected, ignored or patched. The tests import the installed package and pass once a CA bundle is present.

Checked against 1.7.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The offline Cargo registry (Source1) is the crate source for the build; see Build requirements.

Checked against 1.7.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Tests panicked while creating the HTTP client with "No CA certificates were loaded from the system". Fixed by adding the CA certificate BuildRequires; the 92 tests then pass.
- Nested %python_expand in %build broke the per-flavor PYO3_PYTHON export. Fixed by sharing one expansion between the export and the wheel build.
- Install failed because the top-level extension .so is not in the buildroot; the module is installed as the hf_xet/ package directory. Fixed by listing the package directory in %files instead of the .so. Build passes now.
- Duplicate maturin BuildRequires and a comma-bounded version constraint on one line. Fixed by declaring maturin once and splitting the constraint onto separate lines.

Checked against 1.7.0 on openSUSE_Tumbleweed, x86_64.
