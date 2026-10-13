# python-fastar

Version 0.12.0. Maturin/Rust extension, built from the maturin pattern. Last build succeeded.

## Build requirements

- Test-only BuildRequires: `psutil`, `typing_extensions`, `backports.zstd`.
  - Spelled with a dot: the hyphenated `backports-zstd` does not resolve in OBS.
- Maturin constraint is written without the comma form (the comma form was dropped).
- `ExclusiveArch` is limited to the tier-1 architectures.
- `%files` has architecture-specific entries.

## Test changes

- `%pytest_arch` excludes the benchmarks directory.
- `%check` result: 323 passed, 3 xfailed.

## Workarounds

- Cargo registry is extracted offline in `%prep` from `Source1`, so `%build` needs no network access.
- `CARGO_*` and `PYO3_*` environment variables are set in `%build`.
- `%build` calls plain `%pyproject_wheel` once per flavor. A nested `%python_expand` is not used, because it rebuilt every wheel once per flavor.
- Wheels for both flavors (cp313, cp314) come out correct without setting `PYO3_PYTHON`.
- `%license LICENSE` is included.
- License is tagged with the MIT SPDX identifier.

## Failure history

- `backports.zstd` BuildRequires used the hyphenated name, which OBS does not resolve. Fixed by using the dotted name.
- Nested `%python_expand` in `%build` rebuilt every wheel once per flavor. Fixed by using plain `%pyproject_wheel`.
- Missing `%license LICENSE`. Fixed by adding it to `%files`.
- `%files` listed a top-level `fastar.cpython-*.so`, but maturin installs a `fastar/` package directory (the sdist ships a stub `fastar.pyi`). Fixed by listing the `fastar/` directory in `%files`.
