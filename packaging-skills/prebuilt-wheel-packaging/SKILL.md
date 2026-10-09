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
"only four exceptions" list (Exception 3: binary-only upstream, no source
exists) is why taking the wheel as-is is correct here rather than a
workaround to avoid.

`source/` in the workspace is the wheel already unpacked flat (no wrapping
directory) — read `source/*.dist-info/METADATA` for the real summary and
license, and `source/*.dist-info/RECORD` for the exact file list %files
must account for. Do this before touching %files; never guess the layout.

- `%install` is just `%pyproject_install %{SOURCE0}` — it accepts a
  pre-built wheel path directly, no `%pyproject_wheel` step first.
- Arch vs. noarch is decided by the wheel FILENAME's platform tag, and the
  generator already applied it: `-none-any.whl` → `BuildArch: noarch` and
  `%{python_sitelib}`; anything else (`manylinux2014_x86_64`, `win_amd64`,
  ...) → arch-specific and `%{python_sitearch}`. Do NOT change it because
  `WHEEL` says `Root-Is-Purelib: true` — NVIDIA's CUDA wheels say that and
  still ship `.so` files under a `py3-none-manylinux_*` tag; flipping them to
  noarch/sitelib fails rpmlint (`arch-independent-package-contains-binary-or-object`)
  and costs attempts (nvidia-cuda-runtime, nvidia-cuda-nvrtc, nvidia-curand
  all did this). Only override the generator when the filename tag and the
  contents truly disagree, and say which file proves it.
- `%check` for a wheel: there is no source tree, so check the installed
  files. Run any import from a fresh empty directory with
  `PYTHONPATH=%{buildroot}%{$python_sitearch}` (or `sitelib` for noarch) —
  from the build directory the buildroot is not on `sys.path` and the import
  fails with `ModuleNotFoundError` (six NVIDIA packages spent an attempt on
  exactly this). Many of these wheels are NAMESPACE or BINARY-ONLY: `nvidia/`
  has no `__init__.py` and the wheel holds only `lib*.so` and headers, so
  there is nothing to import (`nvidia-nvjitlink`, `nvidia-cufile`). Then
  assert the real payload exists instead, with a path taken from RECORD:
    test -e %{buildroot}%{python_sitearch}/nvidia/cu13/lib/libnvJitLink.so.13
  Never `find %{buildroot}` blindly or rely on a macro inside a `%check`
  shell snippet that RPM has not expanded the way you expect.
- Namespace package directories (`nvidia/`, `nvidia/cu13/`) shared with other
  wheels need `%dir` entries in `%files`, or rpmlint reports
  `directories not owned by a package` (convention 17).
- Prebuilt vendor binaries trip rpmlint rules a source build would not:
  `.so` files without an ELF hash section (`shlib-with-no-hash-section`-style
  badness of 10000 each) are expected for NVIDIA's libraries — add an
  rpmlintrc `addFilter(...)` for them (convention 39) instead of trying to
  rebuild them. Bundled C headers in a CUDA wheel are the SDK's public API
  that dependent extensions compile against: put them in a `-devel`
  subpackage (convention 53) rather than deleting them; only strip headers
  nothing downstream could use.
- License: read `source/*.dist-info/METADATA`'s `License-Expression` or
  `Classifier: License ::` lines, or the packaged `LICENSE`/`NOTICE` file
  if `source/` ships one — the PyPI JSON alone is frequently `FIXME` for
  these releases even when the wheel itself carries a real license file.
- `%files` paths must be exact — read RECORD, don't glob.

## Several flavors, one CPython-locked wheel each (Triton-shaped)

When PyPI publishes `...-cp313-cp313-manylinux_*_x86_64.whl` and
`...-cp314-cp314-manylinux_*_x86_64.whl` and nothing covers both
(convention 49), the automatic wheel switch refuses and you write the spec
yourself:

    Source0:        <name>-%{version}-cp313-cp313-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl
    Source1:        <name>-%{version}-cp314-cp314-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl
    ...
    %install
    %python313_pyproject_install %{SOURCE0}
    %python314_pyproject_install %{SOURCE1}
    %python_clone -a %{buildroot}%{_bindir}/<console-script>
    %python_expand %fdupes %{buildroot}%{$python_sitearch}

There is no `%build`. Get the flavors from `rpmspec -E '%{pythons}'` and
pick one wheel per flavor for the target arch. Each wheel must be in the
checkout (`osc add`); `%python<NNN>_pyproject_install` is used by its
hardcoded name because the shared multi-flavor `%pyproject_install` repeats
one argument for every flavor.
