# python-pycrdt

## Build requirements

- The upstream pyproject uses maturin as build backend with `maturin >= 1.8.2, < 2`. The older spec's `maturin >= 1.4.0` is too old for this version.
- The Rust extension is compiled during the build, so the Rust development package, a C++ compiler and zstd are required. zstd is needed to unpack the bundled crate registry.
- Runtime Requires: `anyio >= 4.4.0, < 5`, `exceptiongroup` with no upper bound (an older upper bound was wrong and was removed), and `typing_extensions >= 4.15.0`.
- Test BuildRequires: `pydantic >= 2.5.2, < 3` is added, because the test suite imports it. The spec does not carry the upstream upper bound on `trio` (upstream allows below 0.35); the reason was not recorded.
- ExclusiveArch was added to the spec. Its architecture list has not been checked against other targets.

Checked against 0.14.8 on openSUSE_Tumbleweed, x86_64.

## Test changes

- `tests/test_model.py` is no longer ignored. It imports pydantic, which is now a test BuildRequires. The earlier skip had no recorded evidence behind it.

Checked against 0.14.8 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The Rust build runs offline from the bundled crate registry archive, with cargo and the Rust flags pointed at it. The bundled vendor archive from the old spec is stale for this version, so it is not used.
- Source0 is the upstream `.tar.gz` tarball. The old spec named a `.tar.xz`.
- In `%build`, the nested `%python_expand` is fixed so each Python flavor's wheel build runs inside its own PYO3_PYTHON setting.

Checked against 0.14.8 on openSUSE_Tumbleweed, x86_64.
