---
name: prebuilt-wheel-packaging
description: Use when the generator's comment says "This release has no sdist and no discoverable source repository" — a closed-source binary-only PyPI wheel with nothing to compile (the common shape for NVIDIA CUDA toolkit components like nvcc, cupti, nvrtc, cudart). Covers reading source/*.dist-info/METADATA and RECORD directly, %install with a raw wheel path, and the import-smoke-test %check.
---

# Prebuilt binary wheel packages (no %build step at all)

When the generator's comment says "This release has no sdist and no
discoverable source repository", `%{SOURCE0}` is the upstream wheel file
itself, not a tarball — there is nothing to compile, and `%prep`/`%autosetup`
do not apply. This is the normal shape of a closed-source binary
redistribution: NVIDIA ships each CUDA toolkit component this way (nvcc,
cupti, nvrtc, cudart, ...) because the actual compiler/runtime binaries have
no public source at all, only a PyPI wheel. The main packaging conventions'
"only three exceptions" list (Exception 3: binary-only upstream, no source
exists) is why taking the wheel as-is is correct here rather than a
workaround to avoid.

`source/` in the workspace is the wheel already unpacked flat (no wrapping
directory) — read `source/*.dist-info/METADATA` for the real summary and
license, and `source/*.dist-info/RECORD` for the exact file list %files
must account for. Do this before touching %files; never guess the layout.

- `%install` is just `%pyproject_install %{SOURCE0}` — it accepts a
  pre-built wheel path directly, no `%pyproject_wheel` step first.
- The generator guessed arch vs. noarch from the wheel's platform tag
  (`-none-any` → noarch/sitelib, anything else → sitearch). Confirm it
  against the actual contents of `source/` — a wheel bundling `.so` files
  is arch-specific regardless of what its tag says.
- `%check` gets an import smoke test (there is no source to run a real
  test suite against), using the import name from `source/*.dist-info/METADATA`'s
  `Name:` field or the top-level directories actually present in `source/`.
- License: read `source/*.dist-info/METADATA`'s `License-Expression` or
  `Classifier: License ::` lines, or the packaged `LICENSE`/`NOTICE` file
  if `source/` ships one — the PyPI JSON alone is frequently `FIXME` for
  these releases even when the wheel itself carries a real license file.
- `%files` paths must be exact — read RECORD, don't glob.
