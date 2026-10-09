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
  It already runs once per Python flavor. NEVER wrap it in `%python_expand`
  (the nested expansion breaks the bytecode-compile step with an
  `IndentationError` — seen on nvidia-cusparselt-cu13 and nvidia-nvjitlink)
  and do not replace it with a hand-written `pip install --root`; neither
  fixes anything. If `%check` fails to import the module after install, the
  cause is almost always the site directory (see the next bullet), not the
  install macro: read the "Installing collected packages" / unpackaged-files
  lines in build.log to see where the files actually went.
- Two separate decisions, and the generator already made both from the wheel:
  * ARCH-SPECIFIC or not comes from the wheel FILENAME's platform tag:
    `-none-any.whl` → `BuildArch: noarch`; anything else
    (`manylinux2014_x86_64`, ...) → no `noarch`, plus `ExclusiveArch:  x86_64`
    (or `aarch64`) for the architecture it was built for.
  * WHICH DIRECTORY the files land in comes from `Root-Is-Purelib` in
    `source/*.dist-info/WHEEL`, NOT from the tag: `true` → `%{python_sitelib}`,
    `false` → `%{python_sitearch}`. NVIDIA's CUDA wheels (`...-py3-none-manylinux_*`)
    say `Root-Is-Purelib: true`, so pip installs them under **sitelib** even
    though they ship `.so` files and are arch-specific. Specs that switched
    these to `%{python_sitearch}` packaged nothing and `%check` failed with
    `ModuleNotFoundError: No module named 'nvidia'` (nvidia-cusparselt-cu13,
    nvidia-nvjitlink, nvidia-cufile all burned attempts on this); the specs
    that went green (nvidia-curand, nvidia-cuda-cupti) use `ExclusiveArch`
    with `%{python_sitelib}`. Use ONE site macro consistently in `%fdupes`,
    `%check` and `%files`, and do not flip it or add/remove `noarch` unless
    the unpacked `source/` proves the generator wrong — read the `WHEEL`
    file, don't guess.
- `%check` for a wheel: there is no source tree, so check the installed
  files. Run any import from a fresh empty directory with
  `PYTHONPATH=%{buildroot}%{$python_sitelib}` (the same site macro the `%files` use) —
  from the build directory the buildroot is not on `sys.path` and the import
  fails with `ModuleNotFoundError` (six NVIDIA packages spent an attempt on
  exactly this). Many of these wheels are NAMESPACE or BINARY-ONLY: `nvidia/`
  has no `__init__.py` and the wheel holds only `lib*.so` and headers, so
  there is nothing to import (`nvidia-nvjitlink`, `nvidia-cufile`). Then
  assert the real payload exists instead, with a path taken from RECORD:
    test -e %{buildroot}%{python_sitelib}/nvidia/cu13/lib/libnvJitLink.so.13
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
