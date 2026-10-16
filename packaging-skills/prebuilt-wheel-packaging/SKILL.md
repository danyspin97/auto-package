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
    file, don't guess. The wheel tag never moves files to sitearch: a purelib
    wheel with `.so` files stays in sitelib with `ExclusiveArch`.
  * SPLIT wheels of one project that fill the same top-level directory
    (`nvidia-cutlass-dsl-libs-base/-core/-cu12/-cu13`): each lists only its own
    files and co-owns shared directories with `%dir` (the skeleton does this from
    the sibling wheels, in any build order; keep it). The tree must live in ONE
    site directory: if a `Root-Is-Purelib: false` wheel shares a top-level
    directory with a `true` one, the skeleton moves the `false` wheel's tree into
    `%{python_sitelib}` in `%install` (`mv`, dist-info included); keep those
    lines and do not edit another package's RECORD.
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
- Prebuilt vendor binaries trip rpmlint rules a source build would not, and
  every NVIDIA wheel package has burned two or three attempts rediscovering
  them one at a time (cudnn, cusparse, cufft, cufile, cusparselt). Do all of
  this in the FIRST pass:
  * Headers: delete them in `%install` —
    `%python_expand rm -rf %{buildroot}%{$python_sitelib}/nvidia/*/include`
    (what the green nvidia-cuda-cupti and nvidia-curand specs do). Nothing
    here compiles against them: a package that needs CUDA to build is itself
    repackaged from its wheel (convention 27), so a `-devel` subpackage buys
    nothing and costs `%exclude` plumbing and extra rpmlint surface.
  * `.so` rpmlint noise (`missing-hash-section` at badness 10000 per file,
    `shared-library-not-executable`, `library-without-ldconfig-post*`) is
    expected for NVIDIA's prebuilt libraries: add `Source99:
    python-<name>.rpmlintrc` containing `addFilter("<rule>.*")` for exactly
    the rules build.log reports (convention 39). Do not try to `chmod`/relink
    vendor binaries. The skeleton already has `Requires: python-base` (rpmlint
    `no-dependency-on python-base`; `%python_subpackages` does not add it): keep it.
  * `%files`: the `%{python_sitelib}/<dist>-%{version}.dist-info/` directory
    claim already owns `licenses/` and everything under it — a second
    `%license` line for a file inside it makes "File listed twice" fail the
    build. Keep ONE of the two. A `%license` path is relative to the build
    tree, never prefixed with `%{buildroot}` (one attempt added it and broke
    the file lookup).
  * Remove pip's `INSTALLER`/`REQUESTED` files from the dist-info in
    `%install` if rpmlint lists them as unpackaged (convention 23).
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

There is no `%build`. Get the flavors from the `%pythons` definition under `/usr/lib/rpm/macros.d` (grep it) and
pick one wheel per flavor THIS PROJECT BUILDS for the target arch — not every
CPython PyPI happens to have a wheel for (onnxruntime was rewritten for
cp311/cp312/cp313/cp314 when only cp313/cp314 are built).
`%python<NNN>_pyproject_install` is used by its hardcoded name because the
shared multi-flavor `%pyproject_install` repeats one argument for every
flavor.

You cannot download the wheels, and a `Source:` whose file is not in the
checkout is rejected before the build ("missing source files"). The pipeline now
downloads the per-flavor wheels itself and writes this spec when every flavor
has a wheel (`use_upstream_wheel` triggers it), so prefer that. If you must
declare them by hand, use the `download_files` OBS service — it is the one
installed here (`/usr/lib/obs/service/`); a `download_file`/`download_url` service
is NOT available and `osc service ra` fails on it (triton's attempt). Put the
full wheel URL (`urls[].url` from the PyPI JSON,
`https://files.pythonhosted.org/packages/<h1>/<h2>/<hash>/<file>.whl`) in each
`Source<N>:` and add:

    <services>
      <service name="download_files" mode="manual"/>
    </services>

The pipeline runs `osc service ra` before the check, so the files arrive on
their own (the check compares the URL's file name with what is on disk). A bare
file name in `Source<N>:` with no URL cannot be fetched by the service.

Delete the source tarball the earlier recipe used from the checkout (a spec
that no longer lists it must not leave it behind to be built from source).
If the service fails, the error is shown to you on the next turn.

## Checks that every prebuilt-wheel spec passes

- `License:` is `SUSE-NonFree` for NVIDIA's redistributable binaries, joined with
  `AND` to the open licences of what else is in the wheel; no `LicenseRef-...`
  spelling (rpmlint: `invalid-license`). Every licence file in the dist-info is a
  `%license` line.
- Bundled libraries do not leak into dependencies: `%global
  __provides_exclude_from ^%{python_sitearch}/.*$` (`%{python_sitelib}` for a
  purelib wheel), and `%global __requires_exclude` for the sonames only an
  optional plugin needs (MPI, UCX, fabric) or that another package of the
  project supplies (then write the explicit `Requires: python-<pkg>`).
- Console scripts and hashed libraries: the pipeline's skeleton reads the
  wheel. Each `[console_scripts]` entry of its `entry_points.txt` already has
  convention 18's `%python_clone`, `update-alternatives`, `%post`/`%postun` and
  `%python_alternative` lines (otherwise `/usr/bin/<script>` is unpackaged), and
  a wheel with auditwheel libraries (`<name>.libs/`, `libfoo-<8 hex>.so.N`)
  already has `%global __requires_exclude ^lib.*-[0-9a-f]{8}.*\.so.*$`. Check
  them, do not duplicate them; rpm honours one `__requires_exclude`, so fold
  another pattern into that line with `|`.
- NVIDIA driver libraries (`libcuda.so.1`, `libnvidia-ml.so.1`,
  `libnvidia-ptxjitcompiler`, `libnvcuvid`) come from the host driver and no
  repository provides them. The skeleton of every compiled wheel already folds
  the driver regex (`depclass::driver_requires_exclude`) into
  `%global __requires_exclude`, and the pipeline adds it itself when OBS reports
  one of them unresolvable (python-flashinfer-python). Never list the driver as
  a `Requires`, never report it as a missing package, never block on it.
- Delete headers, `*.a` archives, test libraries and other architectures' stubs
  in `%install`; list the remaining files in `%files`.
- `%check` imports the module when there is one, and a package whose payload other
  packages import checks that import chain; `test -e <lib>.so` is only for a
  wheel with no module.
- Runtime `Requires:` are `python-<name>`, never `%{python_module <name>}`.
- `rpmlintrc` filters only for warnings the build log names, anchored with
  `/usr/lib(64)?/python`.
