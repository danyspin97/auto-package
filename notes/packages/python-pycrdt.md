# python-pycrdt

## Build requirements

- The upstream build backend is maturin, which must be `>= 1.8.2, < 2`. The build also needs the Rust toolchain, a C++ compiler and zstd. zstd unpacks the bundled crate registry.
- Runtime Requires: `anyio >= 4.4.0, < 5`, `exceptiongroup` with no upper bound, and `typing_extensions >= 4.15.0`.
- Test BuildRequires: `pydantic >= 2.5.2, < 3`, because the test suite imports it. The upstream cap `trio < 0.35` is not carried into the spec.
- ExclusiveArch is set in the spec. Its architecture list has not been checked against other targets.

Checked against 0.14.8 on openSUSE_Tumbleweed, x86_64.

## Test changes

- `tests/test_model.py` is no longer ignored. It imports pydantic, which is now a test BuildRequires.

Checked against 0.14.8 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The Rust build runs offline from the bundled crate registry archive (Source1), with cargo pointed at it. The old bundled vendor archive is stale for 0.14.8 and is not used.
- Source0 is the upstream `.tar.gz` tarball.
- In `%build`, `PYO3_PYTHON` is set inside each Python flavor's own expansion, not once outside it. PyO3 builds against a single interpreter per build, so each flavor needs its own.

Checked against 0.14.8 on openSUSE_Tumbleweed, x86_64.
