//! AI-assisted spec repair — tool-using turn loop only (REDESIGN §2.4).
//!
//! The only backend is the `claude` CLI (`--agent-backend cli`). Vertex AI and
//! direct-API backends are removed in M1 per REDESIGN §5.1.

use std::process::Command;

use serde::Deserialize;

use crate::log::ExtractedLog;

// ── conventions ───────────────────────────────────────────────────────────────

const CONVENTIONS: &str = r#"
openSUSE spec conventions to keep in mind:

1. BuildRequires version constraints go INSIDE the macro with spaces around the operator:
   CORRECT:   BuildRequires:  %{python_module setuptools >= 77}
   WRONG:     BuildRequires:  %{python_module setuptools} >= 77
   WRONG:     BuildRequires:  %{python_module setuptools>=77}

2. Normalized vs. display vs. import name — three different things:
   - the DISPLAY name is what PyPI shows (`MarkupSafe`, `rich-toolkit`);
   - the NORMALIZED name is lowercase with `-`/`.` turned into `_`
     (`markupsafe`, `rich_toolkit`). The dist-info directory is always
     `<normalized>-%{version}.dist-info/`, and the generator already writes it;
   - the IMPORT name is whatever directory actually lands in site-packages,
     and it is frequently neither of the above. Never derive it — look it up.
     Convention 13 says how.
   `%autosetup -n` takes the tarball's top-level directory, which is usually
   but not always the normalized name; the generator sets it from the tarball.

   The RPM package name (what goes inside `%{python_module ...}`) follows the
   DISPLAY name's capitalization, not the normalized one — `%{python_module
   Pillow}`, not `%{python_module pillow}`. This trips people up because pip
   and PyPI treat names case-INsensitively, so a project's own
   pyproject.toml/setup.cfg dependency list frequently spells a dependency in
   lowercase (`dependencies = ["pillow>=10.3.0"]`) even when the canonical,
   OBS-packaged spelling is mixed-case. Copying a name verbatim out of
   `[project.dependencies]` or `install_requires` (a main-dependencies diff,
   or any test-dependency addition) is NOT safe —
   re-check every name you add against its canonical PyPI spelling
   (`https://pypi.org/project/<name>/` or `curl -s
   https://pypi.org/pypi/<name>/json | jq .info.name`) before writing it into
   BuildRequires/Requires. Known mixed-case names include Pillow, PyYAML,
   MarkupSafe, PyJWT, PySide6, PyQt5, PyMySQL, PyNaCl — but don't rely on a
   memorized list, verify each one.

3. Architecture: a package is arch-specific only if it actually COMPILES an
   extension, not merely because C sources are present in the tarball. The
   generator switches to the arch macros whenever it finds a `.c`/`.cpp`/`.cc`/
   `.pyx` file, which is right far more often than not but is still a guess.
   Confirm it: look for `ext_modules=` / `Extension(` in setup.py, a
   `[tool.scikit-build]` section or CMakeLists.txt, `cffi_modules`, or a `.pyx`
   the build actually cythonizes. If there is no compiled extension, put
   `BuildArch: noarch` back and switch `%{python_sitearch}` to
   `%{python_sitelib}` throughout — a pure-Python package installed to sitearch
   fails %files with 'File not found'.

   **CUDA kernel packages are a common false positive**: a package can ship
   `.cu`/`.cpp` source files (or embed CUDA C++ as Python string literals)
   that are compiled at IMPORT or RUN time by NVRTC / `torch.utils.cpp_extension`
   / Triton's own JIT, never during the RPM build itself. No `setup.py`
   `ext_modules=`, no CMakeLists.txt, no cythonized `.pyx` — this is
   `BuildArch: noarch` even though `.cu`/`.cpp` files are present in the
   sdist. Check for an actual build-time compile step before trusting the
   file extensions alone.

   If there IS a compiled extension:

   CRITICAL requirements for arch packages:
   - Use %{python_sitearch} instead of %{python_sitelib} (in %install, %check, %files)
   - Do not set 'BuildArch: noarch'
   - Use %pytest_arch rather than %pytest in %check (using %pytest breaks tests)
   - %fdupes MUST target sitearch: %python_expand %fdupes %{buildroot}/%{$python_sitearch}
     (NOT sitelib — this is a common generator error)

   Common -devel package requirements for compiled extensions:
   - pybind11-devel, USUALLY (see the pybind11 exception just below)
   - %{python_module numpy-devel} (for numpy/arrayobject.h)
   - %{python_module devel} (for Python.h)
   - gcc-c++ for ANY C++ extension, whether built via CMakeLists.txt, a plain
     setup.py `Extension()`/`ext_modules=`, or cffi — the generator only
     reliably adds it for the CMake case, so check for it whenever the
     language is C++ (not C) regardless of build system. Add cmake itself
     only when CMakeLists.txt is actually present.
   - cuda-devel, cuda-runtime (for CUDA extensions — see convention 27)

   Add BuildRequires for any C libraries the extension links against,
   e.g. 'BuildRequires: openssl-devel' for SSL, 'BuildRequires: libffi-devel'
   for cffi. When headers or libraries are missing, use `zypper search --provides`
   or `zypper what-provides` to find the package that provides them (see the
   "Missing C Headers and Libraries" section below for the search pattern).

   When the headers come from a *Python* package, the base package usually
   does not carry them — the -devel flavour does. A build that fails on
   `pybind11/pybind11.h: No such file` usually needs
   `BuildRequires: %{python_module pybind11-devel}`, not
   `%{python_module pybind11}`. The same applies to any
   `fatal error: ...: No such file or directory` naming a header that a
   Python dependency is supposed to provide.

   **pybind11 exception — check how the build actually finds the headers
   before choosing**: pybind11 ships its own headers inside the pip package
   too (`pybind11/include/` in site-packages), and a setup.py that gets its
   include path from `pybind11.get_include()` or `pybind11.__file__` (rather
   than a system include path or CMake's `find_package(pybind11)`) only
   needs the plain `%{python_module pybind11}` — adding `-devel` on top of
   that is harmless but adding it INSTEAD is wrong; only `-devel`, with no
   plain package, leaves `pybind11.get_include()` unable to `import pybind11`
   at all. Read setup.py/CMakeLists.txt to see which lookup it uses before
   picking.

   Remove installed C source files in %install:
     %python_expand rm -f %{buildroot}%{$python_sitearch}/<pkg>/*.c

4. Maturin packages (`build-backend = "maturin"` in pyproject.toml) compile
   Rust extensions inside a Python sdist and need a specific Rust-offline
   pattern on top of the normal Python conventions. Use the
   `maturin-packaging` skill for the full treatment — do not add
   `cargo-packaging` macros or `%{cargo_build}` yourself; maturin drives the
   Rust build via `%pyproject_wheel`.

5. License must be a valid SPDX expression. The generator copies whatever
   PyPI metadata says, which is often unusable:
   - `License: FIXME` means PyPI carried no usable license field. Read
     source/pyproject.toml (`[project] license`), source/setup.py, or the
     LICENSE file itself, and write the SPDX expression it states — e.g.
     `Apache-2.0 WITH LLVM-exception`, `BSD-3-Clause`, `MIT`.
   - Free text that is not SPDX (`BSD-derived (http://www.repoze.org/...)`,
     `Apache Software License`) must be translated. openSUSE also defines
     aliases for licenses with no SPDX id, e.g. `SUSE-Repoze` for the Repoze
     BSD variant; use those rather than inventing an expression.
   - A `License:` field spanning many lines, or containing phrases like
     "Permission is hereby granted" / "Apache License" boilerplate, means the
     generator dumped the entire license text into the tag instead of an
     SPDX id. Replace the whole thing with the short SPDX expression (e.g.
     `Apache-2.0`), never leave the full text in the tag.
   - Also add `%license <file>` to %files naming the actual license file in
     the sdist, if the generator did not.
   - **No LICENSE file anywhere in the sdist at all** (so there is nothing
     for `%license` to point at, even once you know the SPDX id from
     `pyproject.toml`/PyPI metadata): fetch it from upstream as its own
     source rather than leaving `%license` out. Pin to a tag or commit, not
     a moving branch:
       Source1: https://raw.githubusercontent.com/<org>/<repo>/<tag-or-commit>/LICENSE
     then in %prep:
       cp %{SOURCE1} .
     so `%license LICENSE` in %files works normally (real examples:
     `python-passivetotal`, `python-petact`, `python-pname`).

6. Use %pyproject_wheel / %pyproject_install for PEP 517 packages.

7. Always include a %check section with %pytest_arch (C extensions) or %pytest
   (pure Python) and add 'BuildRequires: %{python_module pytest}' for it.

8. CRITICAL: ALL Python packages MUST be built for multiple Python flavors
   (python311, python312, etc.), not just one. Never write code or comments
   that assume a single Python interpreter. The %python_expand and
   %{python_files} macros handle this automatically — this is what
   %python_subpackages is for, and the generator already sets it up. Do not
   confuse this (Python interpreter flavors) with the OBS *build*-flavor
   mechanism described next — they are unrelated and use similar-looking
   macro names.

   A separate, optional Factory pattern splits testing into its own OBS
   *build* flavor: '%global flavor @BUILD_FLAVOR@%{nil}' with a 'test'
   flavor that only runs %check, '%build'/'%install'/'%files' inside
   '%if !%{with test}', and %check inside '%if %{with test}'.

   **This pattern needs a sibling `_multibuild` file, or it silently does
   nothing.** OBS only instantiates a `%{flavor}` other than the default by
   reading a `_multibuild` file next to the spec:
     <multibuild>
       <package>test</package>
     </multibuild>
   Without it, OBS only ever builds the default (non-test) flavor — and
   since %check sits inside `%if %{with test}`, the test flavor never runs
   and %check silently never executes, on every build, with no error at
   all. If you introduce this split, create `_multibuild` in the same
   directory as the spec (a brand-new file — use Write, per convention 14)
   in the same step — never one without the other.

   The auto-package generator does NOT use this split by default — it just
   runs %check inline in the single build, which is simpler and needs no
   `_multibuild` file. Prefer leaving it that way; only introduce the test-
   flavor split for a concrete reason (e.g. test-only BuildRequires so heavy
   they bloat the installed package's dependency closure), and never as a
   copy-paste "standard Factory pattern" reflex.

   If you do introduce the split and the test flavor genuinely cannot run
   on some target (a test-only dependency that doesn't exist on one distro
   version, e.g. SLE vs. Tumbleweed) rather than disabling %check for
   everyone, set `ExclusiveArch: do_not_build` inside that specific
   `%if`/`%ifarch` branch instead — `do_not_build` never matches any real
   architecture, so OBS just skips that one flavor cleanly instead of
   failing the build:
     %if 0%{?suse_version} <= 1600 && "%{flavor}" == "test"
     # this dependency doesn't exist on this target yet
     ExclusiveArch:  do_not_build
     %endif
   This is the same idiom used to gate a whole subpackage on OBS
   Staging-ring conditions — a real, current macro pattern, not a typo for
   a genuine architecture name.

9. Build resources: there are no constraints on RAM or CPU. Always use the
   maximum available parallelism — never add '-j1' or any other job limit to
   make, cmake, or pytest invocations.

10. **Test failures — diagnose "missing" vs. "cannot run here" before touching
   %check.** Conventions 11-13 cover what to do next; this one is only about
   figuring out which situation you're in.

   **Missing dependency** — add it. Look in pyproject.toml
   [project.optional-dependencies] / [dependency-groups] or setup.cfg
   [options.extras_require] for the package, and add
   `BuildRequires: %{python_module <name>}`. If OBS does not have it yet, put
   its RPM name in `unresolvable_packages` and it will be built first — that
   applies to large packages like torch just as much as to small ones
   (convention 27). Only skip the tests instead when the dependency has
   already been tried and failed (convention 15 Step B).

   The same applies when tests are not failing but merely *skipping*: a test
   file that does `try: import aiohttp \n except ImportError: aiohttp = None`
   plus `@unittest.skipUnless(aiohttp, ...)` for an optional integration
   (Django, aiohttp, twisted, and similar framework-integration extras are
   the common case) will silently skip that whole test class if the extra
   is not installed. Installing the extra's own dependencies as BuildRequires
   turns those tests from skipped into actually run — strictly more coverage
   for the same %check section, and worth doing whenever the extra is small.

   **Common missing tools** — tests often shell out to system commands:
   - `FileNotFoundError: 'git'` → add `BuildRequires: git-core`
   - `FileNotFoundError: 'hg'` → add `BuildRequires: mercurial`
   - `FileNotFoundError: 'svn'` → add `BuildRequires: subversion`
   These are system packages, NOT Python packages — never use %{python_module}.
   (Same tool, different symptom: if `git-core` is already present and the
   failure is instead "no repository found," that's not a missing tool at
   all — see convention 48's `git init` case.)

   **"Needs real network" is not always true — check for offline mocking
   first.** Before treating a network-dependent test as unfixable (convention
   11 covers that path), check whether the package's own test suite already
   mocks HTTP instead of calling it live — `BuildRequires:
   %{python_module responses}` (or `vcr`/`vcrpy`, `requests-mock`,
   `httpretty`) in `pyproject.toml`'s test extras means the tests run fully
   offline once that dependency is present. `python-tldextract`,
   `python-tweepy`, and `python-tableauserverclient` all ship offline-mocked
   test suites that only need the mocking library added as a BuildRequires
   (this convention's "missing dependency" case), not a skip.

11. **The test-skip ladder — narrow the skip, never delete the whole suite.**
   Once convention 10 has ruled out "missing dependency," a test can still
   fail because it genuinely **cannot run here**: no network, no GPU, no
   root, no model downloads, no fixture the sdist ships. Skip only what you
   must, never with '|| true' or any other suppression of the exit code, and
   work down this ladder — each rung keeps more coverage than the one below
   it, and an empty %check is the only outcome that catches nothing.

   **A real display is NOT one of these — it is fixable, not a reason to
   skip.** A GUI toolkit's test suite (Qt, Tk, a screen-capture library like
   PyScreeze) can run headless under Xvfb, which OBS workers do have. Add
   `BuildRequires: xvfb-run` and wrap the test invocation:
     %python_expand PYTHONPATH=%{buildroot}%{$python_sitelib} xvfb-run $python -m pytest
   or `xvfb-run %pytest` directly when the macro form works. Real specs
   (`python-QDarkStyle`, `python-PyScreeze`, `python-PyVirtualDisplay`) all
   run their GUI/display tests this way rather than skipping them — only
   fall back to a smoke test or skipping if xvfb-run itself still fails
   (e.g. the suite also needs a real GPU-backed renderer, not just an X
   server).

   (i)   Deselect individual tests:  %pytest -k 'not test_foo and not test_bar'
   (ii)  Ignore a file or directory: %pytest --ignore=tests/contrast
         If the macro mangles a path argument, set the option through the
         environment instead — this always works:
           export PYTEST_ADDOPTS="--ignore=tests/contrast"
           %pytest
         Deleting the directory in %prep also works, but only do that when the
         files must not reach the buildroot either.

         **Common directories to ignore**:
         - Bundled third-party test suites: --ignore=csrc/third_party/pybind11/tests
           (these test the vendored library, not this package — the failure
           usually looks like `ModuleNotFoundError: No module named
           'pybind11_tests'` or another test-helper module that is never
           actually installed)
         - Examples/benchmarks that aren't tests: --ignore=examples --ignore=benchmarks

   (iii) Clear upstream options that do not apply here: %pytest -o addopts=''
   (iv)  Replace the suite with an import smoke test. Do this when the tests
         need something no build worker can have and no package can supply —
         a GPU device, network access, weights downloaded at run time (a
         display does NOT belong on this list — try xvfb-run above first).
         A dependency that is merely not packaged yet is NOT this case:
         report it instead (convention 27). The smoke test still proves the
         extension compiled and the package imports, which is most of what
         %check is for in a distro build:
           %python_expand d=$(mktemp -d) && cd "$d" && PYTHONPATH=%{buildroot}%{$python_sitearch} $python -c "import <import-name>"
         (use $python_sitelib for a pure-Python package). Run it from a fresh
         empty directory — from the source tree, the source package shadows
         the installed one and the test proves nothing.

         **When to use smoke test instead of pytest**:
         - pytest INTERNALERROR (pytest itself crashes) — a pytest version
           incompatibility, broken test setup, or plugin conflict, not
           something a test-path or BuildRequires change will fix
         - All tests require GPU/network (and, for display, xvfb-run already failed)
         - The module imports fine in isolation (this smoke test passes) but
           the FULL test suite still fails with an unrelated-looking
           ModuleNotFoundError — a sign the suite expects a specific runtime
           context (GPU, a torch context, etc.) no build worker has, not
           that anything is actually broken

   (v)   Only if even importing is impossible (the module needs a GPU at
         import time), remove %check and leave a comment saying why.

   Prefer narrowing to deleting: if two tests out of two hundred cannot run,
   deselect those two. A missing conftest.py or test fixture makes the tests
   that use it unrunnable — ignore those files, do not throw away the suite.

   **The same narrowing principle applies along the architecture axis: when
   a failure is specific to the current build architecture, scope the fix
   to that architecture with `%ifarch`/`%ifnarch`, not a blanket skip that
   loses coverage on architectures where the test is fine.** This is an
   extremely common, well-evidenced shape in real specs — recurring causes:
   - **32-bit word-size/overflow bugs in the test itself**, not the code
     under test: `python-cryptography` removes `test_aead.py`/
     `test_ciphers.py` under `%ifarch %ix86 %arm ppc` ("fails with
     OverflowError on 32bit platform"); `python-orjson` deselects
     `test_numpy_array_d1_intp`/`_uintp` only inside `%ifarch %ix86 %arm32`;
     `python-sqlite-vec` skips a `size_t`-derived exact-value assertion the
     same way, with the comment noting the wrong value is a 32-bit overflow
     artifact, not a real bug.
   - **A platform/feature genuinely unsupported on one architecture**:
     `python-debugpy` skips `test_reattach` under `%ifarch aarch64`, citing
     an upstream issue that attach-to-PID isn't supported on ARM; `python-
     lupa` drops its JIT-specific `pkgconfig(luajit)` BuildRequires under
     `%ifnarch ppc64 ppc64le s390x` because LuaJIT itself isn't built for
     those, without restricting the whole package.
   - **Known emulation flakiness/slowness in OBS**, not a real bug:
     `python-black`'s `# sometimes fails on async timing in OBS`,
     `python-binaryornot`'s "tests run too slowly on some architectures".
   In every case, cite the architecture and the reason (an upstream issue
   link when one exists, matching convention 28 Exception 3's evidence
   discipline) directly in the comment, the same way convention 14 asks for
   a bug reference on every patch. This also applies to conditionally
   applying a whole patch, not just deselecting a test: `python-redis`
   applies one patch only `%ifarch s390x` (an endianness-specific fix) while
   its other patches apply unconditionally.

   Write any such comment WITHOUT a '%' character, because RPM expands macros
   inside comments (see the note below the conventions). Write
   `# TODO: tests are not run - all tests require network access`, never
   `# no %check - ...`. Never leave %check empty, never add '|| true', and
   never add '|| [ "$?" = "5" ]'.

12. **pytest configuration pitfalls that look like a code failure but aren't.**
   Before assuming the tests themselves are broken, check whether pytest's
   own configuration is misbehaving — these read as confusing test-count or
   assertion failures but are fixed by a one-line config change, not a patch:

   - **Exit code 5 (no tests collected)**: the tests exist but pytest
     couldn't find them. Read pyproject.toml/setup.cfg to find `testpaths` or
     `tool.pytest.ini_options`, then pass the path explicitly, e.g.
     `%pytest_arch tests/` or `%pytest_arch src/<pkg>/tests/`. (A `setup.cfg`
     using a bare `[pytest]` section instead of `[tool:pytest]` produces this
     same silent-ignore symptom — see convention 43.)
   - **Coverage gates**: ALWAYS read `[tool.pytest.ini_options] addopts`
     (pyproject.toml) or `[tool:pytest] addopts` (setup.cfg) before the first
     build — upstream addopts are written for a git checkout and routinely
     break a package build. `addopts = --cov=<pkg> --cov-report=...` together
     with `fail_under = 100` fails the build on files the build root does not
     exercise, even when every test passes. Do NOT add pytest-cov to make the
     flag work, and do NOT rely on `-p no:cov` — addopts loads the plugin
     before the disable takes effect. Clear the options outright:
       %pytest -o addopts=''
     `--benchmark`, `-n auto` (xdist), `--strict-markers` against plugins you
     have not packaged get the same treatment: clear addopts and pass the
     test path explicitly. Clearing addopts is not suppressing a failure —
     the tests still run and still have to pass.
   - **`filterwarnings = ['error']`** turns EVERY warning into a hard
     failure, including pytest's own "unknown mark" / "unknown ini option"
     warnings. If `[tool.pytest.ini_options]` sets both
     `filterwarnings = ['error']` and an addopts flag or a custom `markers`
     entry that belongs to a plugin (e.g. `--benchmark-*` needs
     pytest-benchmark, a `@pytest.mark.thread_unsafe` marker needs
     pytest-run-parallel), that plugin is not optional the way it would be
     without `filterwarnings = error` — omitting it fails the whole run on
     an unrelated-looking warning-turned-error, not a missing-module
     traceback. Add every plugin implied by addopts/markers as a
     BuildRequires whenever `filterwarnings = ['error']` is set, rather than
     only the ones that produce an obvious ModuleNotFoundError.

13. **Non-test build failures: Source0 mismatches and offline %build errors.**
   Two build-time (not %check-time) failure patterns worth recognizing on
   sight:

   **Source file format mismatch** (error in %prep):
   - `File /home/abuild/rpmbuild/SOURCES/package-1.0.zip: No such file`
   - **Cause**: Source0 line references .zip but actual file is .tar.gz (or vice versa)
   - **Fix**: Read build.log to find actual filename, update Source0 to match

   **%build (not %check) fails with a network error**:
   - `Temporary failure in name resolution`, `Could not resolve host`,
     `Connection timed out` while `%build` is running (not `%check`)
   - **Cause**: OBS build workers have NO network access at all, at any point
     in the build. The build system tried to download something at build
     time — a prebuilt binary (a pinned compiler/toolchain fetched from a
     vendor URL), a CMake `FetchContent`/`ExternalProject_Add` target, or a
     pip resolver trying to hit PyPI because `--no-build-isolation` was
     missing. This is NOT a missing-BuildRequires situation — packaging the
     thing it tried to download does not help if the build script only knows
     how to fetch it itself.
   - **Fix**: Look for the project's own offline switch first — an
     `OFFLINE_BUILD`/`*_OFFLINE=1`-style env var, a CMake option to disable
     the fetching target, or a `SYSPATH`-style override pointing at a
     system-installed copy instead (see Triton's `TRITON_OFFLINE_BUILD` and
     `LLVM_SYSPATH`/`JSON_SYSPATH` for a real example). Never report the
     downloaded thing as `unresolvable_packages` merely because a URL fetch
     failed — confirm first whether it is even packageable as an independent
     RPM (a header-only library the build vendors as source usually isn't).
     A third option when neither an offline switch nor a SYSPATH override
     exists: if the dependency is ALSO already vendored as source inside the
     sdist (check for it under a `third_party/`/`extern/`/`vendor/`
     subdirectory before assuming it isn't), patch the CMake
     `FetchContent_Declare`/`MakeAvailable` call to point at that local
     subdirectory (`add_subdirectory(...)`) instead of git-cloning it.
     `python-sentencepiece` does exactly this for a bundled `abseil-cpp`
     copy, with the comment "build the bundled abseil-cpp directly instead
     of FetchContent git-cloning it (no network in build env)".

14. Tools available — USE THEM:
   - Read: get line-numbered file content (use for build.log, pyproject.toml, setup.py)
   - Grep: search for patterns (use to find test dependencies, entry points, etc.)
   - Edit: modify the spec file or source files under source/
   - Write: create a brand-new file that does not exist yet — a `_multibuild`
     file (convention 8), a new `<name>.rpmlintrc` (convention 39) —
     anything Edit cannot do because there is nothing to diff against yet.
     Never use it to rewrite a file Edit already covers.
   - Skill: load the full detail for a conditional packaging situation this
     prompt only briefly points at — maturin, standalone Rust/Cargo, plain
     C/C++, monorepos, prebuilt wheels, systemd services, i18n, -doc
     subpackages, Django test settings. Each relevant rule below names its
     skill explicitly; invoke it there rather than guessing at the details
     from the one-paragraph summary alone.
   - Bash(rpmspec *): run `rpmspec --parse spec` to validate syntax
   - Bash(osc vc *): add a changelog entry to the package's `.changes` file
     (convention 40) — prefer this over hand-editing the file with Edit/Write,
     since it fills in the date/committer header for you in the exact format
     OBS expects.
   - WebSearch: search the web for missing source repositories or package information
   - Bash(cd source && quilt *): create and manage source patches using quilt.
     QUILT_PATCHES is already set to the OBS checkout directory, so patches land
     there automatically. Workflow for each logical fix:
       cd source && quilt new 0001-short-description.patch
       cd source && quilt add path/to/file.py   # BEFORE editing the file
       # Edit source/path/to/file.py using the Edit tool
       cd source && quilt refresh
     Each patch must be small and self-contained (one logical fix per patch).
     After creating patches, add them to the spec header using Edit:
       Patch0: 0001-short-description.patch
       Patch1: 0002-another-fix.patch
     %autosetup -p1 in %prep applies them automatically — no %patch lines needed.

     **Tag every `PatchN:` line with an openSUSE-standard provenance
     comment directly above it** — this is a SUSE-wide packaging norm (not
     Python-specific), and real specs apply it near-universally:
       # PATCH-FIX-UPSTREAM 0001-short-description.patch -- <one-line reason>, gh#<owner>/<repo>#<N>
       Patch0:         0001-short-description.patch
     `TYPE` is `FIX` (bugfix) or `FEATURE`; `ORIGIN` is `UPSTREAM` (mirrors a
     fix that exists/was accepted upstream — cite the upstream issue/PR/
     commit) or `OPENSUSE` (a local-only workaround with no upstream
     equivalent, e.g. adapting to this distro's install layout). Follow with
     a bug reference whenever one exists — `bsc#NNNNNN`/`boo#NNNNNN` (SUSE/
     openSUSE Bugzilla), a CVE id, or `gh#owner/repo#N`. A patch fixing a
     Python-version-removal issue (convention 38) is almost always
     `PATCH-FIX-OPENSUSE`, since upstream fixes for those usually land as a
     new release rather than a cherry-pickable upstream commit. This same
     citation discipline applies to `%ifarch`-scoped test skips too — see
     the architecture-scoping addendum in convention 11.

   If you need information, READ IT. Don't say "I need to check" without calling Read/Grep.
   NEVER state a structural fact about the package (e.g. "has no tests", "uses cmake",
   "no C extensions") without first verifying it with Read or Grep. Assumption is not
   evidence. If you cannot verify it with the tools available, say so in reasoning.

15. Missing module in %check (e.g. `ModuleNotFoundError: No module named
   'conftest'` or any other import error): a test dependency is absent.
   Read the packaging file (pyproject.toml [project.optional-dependencies] /
   setup.cfg [options.extras_require]) to identify all test deps and add
   them as `BuildRequires: %{python_module <dep>}`.

   **Step A — schedule the dep (first time you see it missing):**
   If the package is not yet in OBS, add its
   RPM name to `unresolvable_packages` so the build system can build it and
   retry. Also add `BuildRequires: %{python_module <dep>}` so OBS installs it
   during the test run once it is available.

   **Step B — skip the tests (only when the dep was tried and could not be
   built):** look for the dep under the "Dependencies that failed to build"
   heading above, or in notes.md. That heading lists packages this run
   actually attempted and failed on; reporting one of them again just restarts
   a loop that has already run once. For those, skip the tests that need them
   with `%pytest -k 'not <test>'` or `--ignore=<path>` and say so in your
   reasoning.

   A dep under "Missing BuildRequires", or one you have not seen fail at all,
   is Step A — report it, however heavyweight it looks.

   If the spec has the wrong RPM package name, fix the name instead.

16. The import name is often NOT the package name — look it up, do not derive
   it. The spec generator normalizes the dist-info directory for you
   (`rich_toolkit-%{version}.dist-info/` is always right), but the module
   directory is whatever the project actually installs, and for a large
   minority of packages that is a different word entirely:
     PyJWT → jwt/        opencv-python-headless → cv2/
     z3-solver → z3/     pillow → PIL/         nvidia-ml-py → pynvml.py
   Before writing any `%{python_sitelib}/…` or `%{python_sitearch}/…` module
   line, confirm the name from one of these, in order of reliability:
     - `source/*.egg-info/top_level.txt` (one name per line — definitive)
     - `[tool.setuptools] packages` / `package-dir` in source/pyproject.toml
     - `packages=[...]` in source/setup.py
     - the top-level directories in source/ that contain an `__init__.py`
   Note the package may install several top-level names (PyYAML installs both
   `yaml/` and `_yaml/`); list every one.

17. For a Python package, %files paths must be exact — never use a shell glob
   on the package's own name (e.g. never write 'outlines_core*' or
   'Package-*'). The standard entries:
   - Module directory:    %{python_sitelib}/package_name/
   - dist-info:          %{python_sitelib}/package_name-%{version}.dist-info/
   - C extension module: %{python_sitearch}/package_name/
   `package_name` above stands for the IMPORT name from convention 16, not the
   PyPI name. A trailing '*' risks claiming unrelated files and causes 'file
   listed twice' errors. If you are unsure of the exact path, grep build.log
   for the `BUILDROOT/` paths rpm reported — an 'Installed (but unpackaged)'
   error lists every file it found, which is exactly the set %files must
   account for.

   This is specific to a package's own name and does not forbid every glob
   ever: a versioned shared library's SONAME entry
   (`%{_libdir}/lib<name>.so.*`) is a real, standard openSUSE idiom for a
   plain C/C++ package — see the `plain-c-cpp-packaging` skill — because the
   exact patch version is not something to hardcode. The rule above is about
   not guessing at the package's own directory name, not about globs in
   general.

   A `py_modules=[...]` project (setup.py/setup.cfg) installs bare `.py`
   files at the top of site-packages instead of a package directory — e.g.
   `nvidia-ml-py`'s only module is `%{python_sitelib}/pynvml.py`, not a
   `pynvml/` directory. Check `source/*.egg-info/top_level.txt` either way:
   it lists the installed name whether it is a directory or a single file,
   but only the source (`py_modules=` vs `packages=`) tells you which form
   to write in %files.

   A namespace package (`google/`, `cuda/`) split with `%dir` plus an
   explicit `__init__.py` line (rather than owning the whole directory)
   needs the matching `__pycache__/__init__.*.pyc` listed too — the
   directory's `%dir` entry does not imply ownership of files another
   subpackage generates inside it, and rpm reports the compiled bytecode as
   a separate 'Installed (but unpackaged)' file if it is left out.

18. **Console-script entrypoints must use the openSUSE `python_alternative`
   pattern.** If the package declares [project.scripts] or
   entry_points['console_scripts'] in its pyproject.toml/setup.cfg, each script
   MUST use this pattern. Check pyproject.toml or
   setup.cfg with Read/Grep to find all declared scripts, then add ALL SEVEN
   components below for each script named <script>.

   CRITICAL: ALL components are MANDATORY. Missing ANY ONE will cause build failure:

   Component 1 — In %install (after %pyproject_install, one line per script):
     %python_clone -a %{buildroot}%{_bindir}/<script>

   Component 2 — Also at the end of %install (once, not per script):
     %python_expand %fdupes %{buildroot}/%{$python_sitelib}
     (or %{$python_sitearch} for arch packages)

   Component 3 & 4 — Add these package-level requires (once, not per script):
     Requires(post):   update-alternatives
     Requires(postun): update-alternatives

   Component 5 — In %post section (one %python_install_alternative per script):
     %post
     %python_install_alternative <script>

   Component 6 — In %postun section (one %python_uninstall_alternative per script):
     %postun
     %python_uninstall_alternative <script>

   Component 7 — In %files %{python_files} (one line per script):
     %python_alternative %{_bindir}/<script>

   Example full %build/%install/%post/%postun/%files for a package with 'khard' script:

     %build
     %pyproject_wheel

     %install
     %pyproject_install
     %python_clone -a %{buildroot}%{_bindir}/khard
     %python_expand %fdupes %{buildroot}/%{$python_sitelib}

     %post
     %python_install_alternative khard

     %postun
     %python_uninstall_alternative khard

     %files %{python_files}
     %{python_sitelib}/khard/
     %{python_sitelib}/khard-%{version}.dist-info/
     %python_alternative %{_bindir}/khard

   Missing %python_clone causes 'Installed (but unpackaged) file(s) found' for
   the /usr/bin/ paths. Missing %python_expand %fdupes causes duplicate-file
   errors. Missing Requires(post/postun) causes post-install failures. Missing
   %post/%postun causes update-alternatives errors. Missing %python_alternative
   in %files causes file conflicts. The generator often omits some or all of these —
   you must verify and add all seven components.

19. RPM macros use single '%', never '%%'. For example: %check, %install,
   %{version}, %{python_module foo}. Only use %% when you need a literal
   percent character in the spec file output (which is rare).

20. Python macros: ONLY use macros documented in the openSUSE wiki page
   "Packaging/Python" (https://en.opensuse.org/openSUSE:Packaging_Python).
   Do NOT invent macros. If you are unsure whether a macro exists, do not
   use it — fall back to an explicit shell command instead.

IMPORTANT: Do NOT add comments that reference RPM macros (e.g. `# uses %pytest`,
   `# changed %install`, `# removed %check`). RPM expands macros inside comment
   lines, which can make the spec fail to parse with cryptic errors like
   `Unknown tag: set +x`. Record your reasoning in the `reasoning` response field
   instead — it is kept in the run log and in notes.md, where the next attempt
   will read it.

21. BuildRequires takes ONE simple constraint — not PEP 508. A requirement
   string copied out of pyproject.toml usually will not work. Reduce it to a
   single `name >= version` (or a bare name).

   Common WRONG patterns and their fixes:

   WRONG: BuildRequires: %{python_module maturin >= 1.14.1, < 2.0}
   RIGHT: BuildRequires: %{python_module maturin >= 1.14.1}

   WRONG: BuildRequires: %{python_module numpy ~= 2.0}
   RIGHT: BuildRequires: %{python_module numpy >= 2.0}

   WRONG: BuildRequires: %{python_module setuptools>=77}
   RIGHT: BuildRequires: %{python_module setuptools >= 77}
   (spaces required around operator per convention 1)

   WRONG: BuildRequires: %{python_module numpy >= 2.0; python_version>="3.9"}
   RIGHT: BuildRequires: %{python_module numpy >= 2.0}

   Rules:
   - `!=` is not supported at all. `>=0.1.6,!=0.1.8,!=0.1.8.post0,<0.2`
     becomes `>= 0.1.6`. Never leave a `!=` in any BuildRequires line.
   - Multiple comma-separated constraints are not supported: keep the `>=`
     lower bound and drop the rest. `>=0.9.8,<0.10.0` becomes `>= 0.9.8`.
   - `~=` (compatible release) is not supported: `~= 2.0` becomes `>= 2.0`.
   - Environment markers (`;`) are not supported and make the spec fail to parse:
     drop everything from the `;` onwards. If the marker means the dep is
     genuinely optional on this platform, leave the dep out entirely.
   - Extras in brackets are not supported: `requests[socks]` becomes
     `requests`; add the extra's own dependencies separately if the build
     needs them.
   - Spaces around operators are REQUIRED (convention 1): `>=77` must be `>= 77`.

   Package name capitalization MATTERS — see convention 2. Do not trust the
   casing used in a `requirements`/`dependencies` list; verify the canonical
   PyPI spelling for every name you write.

   This whole rule is about the `%{python_module ...}` macro's
   argument specifically — its internal parser is what rejects commas,
   `!=`, `~=`, and `;`. An ordinary system-package `BuildRequires` (no
   `%{python_module}` wrapper) uses plain RPM dependency syntax, which DOES
   support multiple comma-separated bounds: `BuildRequires: robin-map-devel
   >= 1.3.0, robin-map-devel < 2.0.0` is valid and correct as-is — do not
   "simplify" it down to one bound the way you would inside
   `%{python_module ...}`.

22. Dynamic version and metadata providers need their own BuildRequires, and
   they are not listed in `[build-system] requires`. Symptoms are an import
   error from inside the build backend, or a version that resolves to 0.0.0.
   Read pyproject.toml before the first build and look for:
   - `[tool.setuptools_scm]`, or `dynamic = ["version"]` with a setuptools_scm
     provider, or a `setup.py` that calls `setuptools_scm.get_version()`. The
     sdist has no `.git`, so add `BuildRequires: %{python_module setuptools_scm}`
     (underscore — the OBS package is `python3XX-setuptools_scm`; the
     hyphenated `setuptools-scm` spelling resolves to nothing and fails
     dependency resolution with an empty build log)
     AND export the version in %build and %install:
       export SETUPTOOLS_SCM_PRETEND_VERSION=%{version}
   - `metadata.version.provider` / `metadata.readme.provider` under
     `[tool.scikit-build]`. Each provider is a separate package that must be a
     BuildRequires of its own — e.g. `scikit_build_core.metadata.setuptools_scm`
     needs setuptools_scm, and the `fancy_pypi_readme` provider needs
     `%{python_module hatch-fancy-pypi-readme}`. Add every provider named in
     the file in one edit; discovering them one failed build at a time costs
     an attempt each.
   - A custom build backend's own `get_requires_for_build_wheel()` can return
     dependencies that appear NOWHERE in `[build-system] requires` — that
     list is only what pip needs to invoke the backend at all, not what the
     backend decides it needs once running (which can depend on the
     environment: available CUDA_HOME, an installed torch version, etc.).
     `%pyproject_wheel` builds without network/build-isolation, so anything
     the backend's own code imports must already be a BuildRequires. When
     `[build-system] requires` looks complete but the build still fails on an
     import from inside a `build_backend.py`/backend module, read that
     module's own source (not just pyproject.toml) to find what it actually
     imports — do not assume the static requires list is the full picture.

23. Stray top-level files in site-packages: a package can install files at the
   top of site-packages that are not part of its library and must not be
   shipped. Three sources, all handled the same way:
   - build infrastructure listed in `[tool.setuptools] py-modules`
     (pyproject.toml) or `py_modules` (setup.py/setup.cfg) — names like
     `build_backend`, `build_utils`, `_build`;
   - usage examples and scratch scripts — `example.py`, `demo.py`, `test.py`,
     a top-level `conftest.py`;
   - anything else that appears as a bare `.py` in an
     `Installed (but unpackaged) file(s) found` list.
   Owning a generic name like `example.py` at the root of site-packages would
   collide with every other package that does the same, so remove it rather
   than adding it to %files. Remove each one (and its `__pycache__` bytecode)
   in `%install`:

     %python_expand rm -f %{buildroot}%{$python_sitelib}/build_backend.py
     %python_expand rm -f %{buildroot}%{$python_sitelib}/__pycache__/build_backend.cpython-*.pyc

   Always remove both the `.py` file and its corresponding `__pycache__` entry;
   removing only the `.py` leaves orphan `.pyc` files that trigger the same error.

24. rpmlint devel-file-in-non-devel-package: Each bundled C/C++ header
   (`.h`, `.hpp`) in a non-devel package generates a `devel-file-in-non-devel-package`
   rpmlint error (Badness: 50). Packages that bundle 3rdparty C/C++ libraries
   under `data/`, `3rdparty/`, `include/`, or similar subdirectories can easily
   accumulate thousands of headers, pushing total rpmlint badness above the 999
   threshold and failing the build. Check the rpmlint output in the build log
   for `devel-file-in-non-devel-package` warnings. If present, remove the
   offending directories in `%install`:

     %python_expand rm -rf %{buildroot}%{$python_sitelib}/<pkg>/data/

   These bundled C/C++ sources are build-time artifacts that have no use at
   runtime in the RPM environment (they require CUDA, GPU, etc.) and should
   always be stripped.

25. rpmlint badness is a budget: the build fails when the TOTAL exceeds 999,
   so the fix is whichever group of files contributes the most, not the first
   error in the log. Find the line `Badness NNNN exceeds threshold 999` and
   read the errors above it with their per-error badness. The usual big
   spenders, in descending order:
   - `wrong-script-interpreter` / `env-script-interpreter` on files that are
     not meant to be executed at all — test fixtures, helper scripts, files
     with a templated `#!<<PYTHON>>` shebang. Badness 490 each adds up in
     four files. Delete the directory in %install rather than patching each
     shebang:
       rm -rf %{buildroot}%{$python_sitelib}/<pkg>/tests/
       rm -rf %{buildroot}%{$python_sitelib}/<pkg>/scripts/
   - `devel-file-in-non-devel-package` (convention 24), Badness 50 each.
   Removing test and script directories from the installed package is normal
   openSUSE practice — they are not part of the library's runtime surface.

26. Source taken from a repository, not from a PyPI sdist: when PyPI publishes
   only wheels, the tarball is a snapshot of the upstream repository, and it
   does not have an sdist's shape. Check for these before the first build:
   - **The project may be in a subdirectory.** A monorepo tarball has no
     pyproject.toml at the top; the packaging files sit in something like
     `cuda_python/` or `python/` or `src/<pkg>/`. The generator finds that
     directory and has already put `cd <subdir>` at the top of %build and
     %install, with `%autosetup -n` still pointing at the tarball's top-level
     directory — check it picked the right one. If the archive holds several
     projects they are listed above, and convention 30 governs the layout.
   - **The version is not baked in.** A repository checkout has no PKG-INFO
     and often no `.git`, so a dynamic version resolves to 0.0.0 or the build
     backend fails outright — convention 22 covers the fix.
   - **Extra top-level content.** CI configuration, docs, benchmarks and
     vendored third-party trees come along with a repository archive and none
     of it belongs in the RPM; conventions 22 and 23 cover removing it.

27. **ANY dependency missing from OBS is something to BUILD, not something to
   work around.** This is the CORE PRINCIPLE of the entire packaging system.
   Convention 28 lists the only exceptions.

   Reporting it in `unresolvable_packages` is how it gets packaged: the build
   system queues it, builds it, and retries this package afterwards. That is
   the whole job.

   This applies to ALL dependency types without exception:
   - **Python packages**: torch, jax, transformers, pydantic — report them ALL
   - **C/C++ libraries**: libfoo, libbar-devel — if zypper can't find them, report them
   - **System tools**: cmake, ninja, specialized build tools — report if missing
   - **CUDA libraries**: cuda-devel, cuda-runtime, CUDA Python packages
   - **ANY other dependency**: regardless of type, size, or complexity

   **Size and complexity DO NOT MATTER.** torch, jax and the rest of the ML stack
   are in scope exactly like any other dependency. Do not treat a big or awkward
   package as out of reach. Do not disable a feature or drop a test suite merely
   because its dependency is not in OBS yet. The build system WILL package it.

   **Keep the spec honest:** Leave all `BuildRequires` in place (Python packages,
   C libraries, system tools, everything) so the build uses them once they exist.
   Never remove a BuildRequires line just because the package is not available yet.

   **Special case — CUDA libraries and packages:**

   Packages that link against or provide bindings to CUDA libraries (like
   cuda-python, nvmath-python, etc.) MUST declare CUDA as a BuildRequires:

   1. Check the source for CUDA references: #include <cuda.h>, libcuda.so,
      libcudart.so, find_package(CUDAToolkit), etc.
   2. If found, add these BuildRequires:
      BuildRequires:  cuda-devel
      BuildRequires:  cuda-runtime
   3. Also add CUDA to `unresolvable_packages` if OBS reports it as unresolvable
      — it will be built and made available
   4. NEVER skip packages that require CUDA libraries

   CUDA Python ecosystem libraries are all open source and available on GitHub.
   When encountering any of these as dependencies, report them for packaging:
   cuda-python (cuda.core, cuda.bindings, cuda.pathfinder), cuda-compute,
   numba-cuda-mlir, cuda-tile, nvmath-python (nvmath.bindings), nvshmem4py,
   and similar NVIDIA CUDA Python libraries.

   **Clarification, not an exception — dlopen/dlsym at runtime is not a
   build-time dependency at all**: some packages talk to CUDA/cuFile/NCCL
   through their own minimal, ABI-compatible struct/typedef stubs and load
   the real symbols with `dlopen`/`dlsym` at runtime, rather than
   `#include`ing the real headers or linking `libcudart`/`libcufile`/
   `libnccl` at build time. Grep for the actual `#include` targets and the
   link libraries (`target_link_libraries` / `-lcudart` / CMake
   `find_package(CUDAToolkit)`) before assuming CUDA references anywhere in
   the source mean a build-time dependency — a `dlopen("libcudart.so", ...)`
   call needs neither `cuda-devel` nor `cuda-runtime` to build. Reporting
   them anyway when they are not linked costs a large, unnecessary build of
   the entire CUDA toolkit for nothing. This is not "working around" a
   dependency (convention 28's exceptions) — there never was a build-time
   dependency to report.

   The same dlopen shape appears outside CUDA too — any ctypes wrapper
   around a system `.so` with no build-time linking (e.g. a Python binding
   that `ctypes.CDLL()`s `libmpv.so` at import time). There the correct
   `Requires` is NOT a hardcoded package name (`libmpv2` breaks on the next
   SONAME bump) — resolve it dynamically at build time instead, against
   whatever package currently owns the file:
     BuildRequires:  mpv-devel
     Requires:       %(rpm -q --queryformat "%%{NAME}" -f $(readlink -f /usr/lib*/libmpv.so))
   The `-devel` BuildRequires gets you the unversioned `.so` symlink to
   resolve against at build time; the shell-expanded `Requires` binds to
   whatever versioned runtime package actually provides it, so the
   dependency survives a SONAME bump without a spec edit (real example:
   `python-python-mpv`).

   **A `noarch` package can still need `ExclusiveArch` if a hard runtime
   dependency is itself arch-limited.** A pure-Python package with no
   compiled code of its own is not automatically buildable everywhere: if
   it `Requires` (or `BuildRequires`s) something that OBS only builds on
   specific architectures — a CUDA library, an ML runtime like
   onnxruntime, a GPU binding — this package becomes unresolvable on every
   other architecture unless it declares the same restriction. `python-
   magika` is `BuildArch: noarch` but sets `ExclusiveArch: x86_64 aarch64`
   because it `Requires: python-onnxruntime`, which OBS only builds on
   those two, with the comment explaining exactly this. Check the
   `ExclusiveArch`/architecture support of every hard runtime dependency
   you add, and propagate the same restriction to this spec if it has one —
   do not assume `noarch` means architecture is irrelevant.

28. **The only three exceptions where working around a dependency (rather
   than reporting it per convention 27) is justified.** All three need
   EVIDENCE from the current run or from upstream, not a guess about how
   hard something looks. If you have not seen it fail in THIS run
   (Exceptions 1-2) or confirmed it upstream (Exception 3), report it
   instead of reaching for one of these.

   Exception 1: The dependency has already been tried and FAILED — it is listed
   above under "Dependencies that failed to build", or notes.md records the attempt.
   Convention 15 Step B covers this; skip the tests that need it and say why.

   Exception 2: The tests need something no build worker has and no package can
   supply: a GPU device, network access, credentials, model weights downloaded
   at runtime. That is a property of the build environment, not a missing
   package. Convention 11 rung (iv) covers it. A display is NOT in this list —
   `BuildRequires: xvfb-run` supplies one; see convention 11's "Cannot run here"
   section before treating GUI tests as unrunnable.

   Exception 3: The dependency is binary-only upstream — no source exists to
   build it from, and the maintainers have said so explicitly (e.g. NVIDIA's
   CUTLASS maintainers confirming CuTeDSL's compiled artifacts, like
   libCutlassIRPythonCAPI.so and the _cute*.so extension modules, ship only in
   the PyPI wheel and will not be open-sourced: see
   https://github.com/NVIDIA/cutlass/issues/2297). When that is true — not
   merely suspected — taking the prebuilt binary from the upstream PyPI wheel
   is the correct approach, not a workaround to avoid. Cite the evidence (an
   upstream maintainer statement, issue, or release note) in your reasoning
   before relying on this exception; do not invoke it just because building
   from source looks hard.

29. Missing source code: when the build fails because a package has no source
   tarball on PyPI and no repository URL in its metadata, search for it
   directly. Use WebSearch with queries like:
   - "site:github.com <package-name> NVIDIA" for NVIDIA CUDA packages
   - "site:github.com <package-name> python" for other packages
   - "<package-name> github repository"
   Look for:
   - Official repositories (nvidia/*, NVIDIA/*, or the project's organization)
   - Releases or tags matching the package version
   - Monorepo structures where the package is a subdirectory
   Once found, report the GitHub repository URL and the specific tag/release
   in your reasoning, and set action to "blocked" with why: "source not on
   PyPI, available at https://github.com/..." so the build system can fetch
   it from there.

30. Monorepos: when the "Other projects in this archive" section above lists
   more than one project, package ALL sibling projects as subpackages of this
   one spec — not one project built and the rest thrown away, and not a
   separate OBS package per subdirectory. Use the `monorepo-packaging` skill
   for the full treatment (subpackage shape, the `%python_subpackages`
   single-invocation limitation, when an umbrella/metapackage is right
   instead of re-vendoring an independently-released sibling).

31. **rpmlint compliance**: Follow rpmlint rules when writing spec files to
   avoid build failures and extra review cycles. Common issues:

   - **summary-too-long** (Badness: 200): Summary line must be SHORT and
     concise, not a complete sentence or detailed explanation.
     - Bad:  `ml_dtypes is a stand-alone implementation of several NumPy dtype extensions used in machine learning`
     - Good: `NumPy dtype extensions for machine learning`
     Keep summaries under ~60 characters when possible. A summary is a
     phrase, not a sentence — no trailing period.

   - **description-line-too-long**: Wrap description lines at 80 characters.

   - **no-manual-page-for-binary**: If the package installs binaries but has
     no man pages, this warning is expected and acceptable.

   - **devel-file-in-non-devel-package**: Already covered in convention 24.

   Before finalizing a spec, mentally check these rules. The goal is to
   produce a spec that passes rpmlint on the first build, not one that needs
   a second round just to fix linting errors.

32. **Prebuilt binary wheel packages** (no %build step at all): when the
   generator's comment says "This release has no sdist and no discoverable
   source repository", `%{SOURCE0}` is the upstream wheel file itself, not a
   tarball — there is nothing to compile, and `%prep`/`%autosetup` do not
   apply (the normal shape for closed-source binary redistribution, e.g. each
   NVIDIA CUDA toolkit component). Use the `prebuilt-wheel-packaging` skill
   for the full treatment (`%install`/`%check`/License from
   `source/*.dist-info/`, arch detection). Convention 28 Exception 3 is why
   taking the wheel as-is is correct here rather than a workaround to avoid.

33. **Plain C/C++ packages** (no Python, no Cargo — just CMakeLists.txt or
   configure.ac): a third ecosystem, much less uniform than the other two.
   The generator only picked the build system and the `%autosetup -n`
   directory; everything else is a TODO for you to fill in from `source/`.
   Use the `plain-c-cpp-packaging` skill for the full treatment (shared-
   library SONAME subpackage split, `%ldconfig_scriptlets`, CMake/autotools
   `%build`/`%check` patterns, `%files` layout).

34. **Optional extras are `Recommends`, not a build-blocking dependency.**
   Convention 24 says any MISSING package should be built — but that applies
   to dependencies the package actually needs to build, import, or pass its
   test suite. `pyproject.toml`'s `[project.optional-dependencies]` /
   `extras_require` frequently lists integrations the package only uses if
   present at runtime (a faster JSON backend, `argcomplete` shell completion,
   an alternate driver) that nothing in `%build` or `%check` imports. Real
   openSUSE specs handle this with a plain `Recommends:` line — not
   `BuildRequires`, not `Requires`, and not `unresolvable_packages` — e.g.
   `python-diffoscope` recommends `python3-argcomplete` and `python3-defusedxml`
   for extras it never imports during its own test run, and `eric` recommends
   a dozen optional integrations (`python3-pyenchant`, `python3-pylint`,
   `python3-rope`, ...) the same way. This is a separate case from convention
   10/15's test-dependency handling: those extras ARE imported by `%check`
   and so must be real `BuildRequires` (Step A: report if missing). Ask which
   situation applies before adding a dependency for an extra:
   - `%check`/`%build` actually imports it → `BuildRequires` (convention 15).
   - Nothing in this build imports it, it only improves behavior for a user
     who installs it later → `Recommends: %{python_module <name>}` (or the
     plain package name for a non-Python tool the package shells out to
     optionally). Never add it to `unresolvable_packages` merely because it
     is missing — a `Recommends` on something not yet in OBS is harmless and
     does not block this package's build at all, unlike a `BuildRequires`.

35. **Systemd service files**: when the package is actually a background
   daemon/server (not most Python libraries) and ships or needs a `.service`
   unit, a dedicated system user, or a runtime directory under `/run` — a
   `*.service` file in the sdist, a `--daemon` mode, an existing sibling
   package doing this — use the `systemd-service-files` skill for the full
   treatment (`%service_add_*`/`%service_del_*` scriptlets, `sysusers.d` for
   the daemon's own user, `tmpfiles.d` for its runtime directories, Source
   placement, %files). Do not invent any of this for a plain library or CLI
   tool.

36. **Translations / gettext `.mo` files (i18n)**: a package that installs a
   `locale/` (or similarly named) directory of compiled `.mo` files needs
   `%find_lang`/`%python_find_lang` to generate the file list, not a
   hand-written %files entry per language directory. Symptom: `Installed
   (but unpackaged) file(s) found` naming `.mo` files under `LC_MESSAGES/`.
   Confirm first with `find %{buildroot} -name '*.mo'` — do not apply
   speculatively to a package with no compiled translations. Use the
   `gettext-i18n-packaging` skill for the full treatment, including the
   directory-rename workaround when translations live under a non-standard
   directory name.

37. **`-doc` subpackage for a package's own Sphinx/generated documentation**:
   worth doing when upstream itself treats docs as a shipped artifact (a
   `docs` extra in `[project.optional-dependencies]`, a Factory sibling
   package already doing this, or the task explicitly asks for it) — not
   speculatively just because a `docs/` directory exists in the sdist; most
   packages ship one without expecting a subpackage. Use the `doc-subpackage`
   skill for the full treatment (sphinx-build invocation, %fdupes
   deduplication, the always-noarch subpackage declaration).

38. **Removed/renamed Python stdlib APIs**: the single most common reason an
   upstream test suite or the package's own code fails on Tumbleweed/SLE's
   current Python but passed on whatever upstream last tested against —
   Python routinely removes APIs a whole release cycle after deprecating
   them, and a large fraction of PyPI packages lag behind. This is a source
   patch (quilt, convention 14), not a spec change, and not a missing
   dependency — do not report these as `unresolvable_packages`. Recognize
   the traceback and apply the known replacement:

   | Removed API                                | Symptom                                          | Replacement |
   |---------------------------------------------|---------------------------------------------------|-------------|
   | `pkg_resources` (setuptools, deprecated)     | `ModuleNotFoundError: No module named 'pkg_resources'` or a deprecation warning turned error | `pkg_resources.iter_entry_points(group)` → `importlib.metadata.entry_points(group=group)`; `pkg_resources.get_distribution(x).version` → `importlib.metadata.version(x)` |
   | `distutils` (removed Python 3.12)            | `ModuleNotFoundError: No module named 'distutils'` | `from distutils.X import Y` → the `setuptools`-vendored equivalent, or `sysconfig`/`packaging` for version/platform helpers — check what the specific submodule did before picking a replacement |
   | `imp` (removed Python 3.12)                  | `ModuleNotFoundError: No module named 'imp'`     | `imp.load_source`/`imp.new_module` → `importlib.util` equivalents (`importlib.util.spec_from_file_location` + `module_from_spec`) |
   | `inspect.getargspec` (removed Python 3.11)   | `AttributeError: module 'inspect' has no attribute 'getargspec'` | `inspect.getfullargspec` (closest drop-in) or `inspect.signature` |
   | `unittest.makeSuite` (removed Python 3.13)   | `AttributeError: module 'unittest' has no attribute 'makeSuite'` | `unittest.TestLoader().loadTestsFromTestCase(...)` — or, if it's only used by legacy `if __name__ == '__main__':` manual-run scaffolding that pytest never calls, just delete that block rather than porting it |
   | `ast.Str`/`ast.Num`/`ast.Bytes`/`ast.NameConstant` (removed Python 3.12) | `AttributeError: module 'ast' has no attribute 'Str'` | All of these unify into `ast.Constant`; check `.value` instead of `.s`/`.n` |
   | `configparser.SafeConfigParser`/`.readfp` (removed Python 3.12) | `AttributeError: module 'configparser' has no attribute 'SafeConfigParser'` | `SafeConfigParser` → `ConfigParser`; `.readfp(f)` → `.read_file(f)` |
   | `collections.Iterable`/`.Mapping`/`.Callable`/etc. (removed Python 3.10) | `AttributeError: module 'collections' has no attribute 'Iterable'` | Import from `collections.abc` instead of `collections` |
   | `asyncio.get_event_loop()` with no running loop (behavior tightened progressively since 3.10) | `DeprecationWarning`/`RuntimeError: There is no current event loop` | `asyncio.get_event_loop_policy().get_event_loop()` or bare `get_event_loop()` outside a running loop → `asyncio.new_event_loop()` (and `asyncio.set_event_loop()` if the result needs to be the thread's loop) |
   | `unittest` deprecated assertion aliases (removed Python 3.12) | `AttributeError: 'TestCase' object has no attribute 'assertEquals'` (or `assertRaisesRegexp`/`assertRegexpMatches`/`failUnlessEqual` etc.) | Drop the deprecated alias for its modern name: `assertEquals`→`assertEqual`, `assertRaisesRegexp`→`assertRaisesRegex`, `assertRegexpMatches`→`assertRegex`, `failUnless*`/`failIf*`→the plain `assert*` form. A simple `sed` rename across the test file is usually the whole fix (real example: `python-fb-re2`'s `assertRaisesRegexp`→`assertRaisesRegex`) |

   **`versioneer.py` is a common carrier for several of these at once**: many
   packages vendor a verbatim copy of the `versioneer.py` script for git-based
   version detection, frozen at whatever version was copied in — if a
   traceback's file is `versioneer.py`, check it for `SafeConfigParser`,
   `readfp`, or other entries in this table before assuming the package's own
   code is at fault.

   This list is not exhaustive — when a traceback names a stdlib module
   attribute that does not exist, search for "<module>.<attribute> removed
   python" before assuming it is package-specific; these removals are
   announced in the Python changelog and the fix is almost always a
   one-to-one rename, not a redesign.

39. **`rpmlintrc` for a genuine rpmlint false positive**: conventions 24/25
   cover removing files that legitimately should not ship. When rpmlint
   flags something that IS correct to ship — a bundled example header that
   is not a real development header, a script whose interpreter line is
   intentional — add a per-package filter instead of fighting the content:
     Source99:       <name>.rpmlintrc
   with the file itself (Write, since it is new) containing one `addFilter`
   call per suppressed warning, anchored as tightly as the regex allows:
     addFilter("devel-file-in-non-devel-package.*/usr/lib/python3.*/site-packages/<pkg>/examples/SampleClass.h")
   Only reach for this when the flagged content is correct and intentional —
   never to silence a warning about something that actually should be
   removed or fixed (that is convention 24/25's job), and never for the
   `summary-too-long`/badness-budget items convention 31 already tells you
   to just fix directly.

40. **Update the `.changes` file for any spec edit meant to fix this build.**
   Only the final spec state gets committed to OBS (one `osc ci` after the
   build succeeds) — there is no separate step that writes a changelog entry
   for you, so if you do not add one during the turn that makes the fix, the
   committed package ends up with no record of what changed and why.
   Whenever you edit the spec (or add/refresh a quilt patch) to address the
   reported failure, also add an entry to `<name>.changes` in the same
   checkout, in the same turn:
     osc vc -m "<one-line summary of this fix>"
   `osc vc` reads the committer identity from oscrc and writes a correctly
   formatted, datestamped entry at the top of the file — matching the
   `----...` separator / `<date> - <name> <email>` header / `- ` bullet
   structure already in every `.changes` file — so prefer it over hand-
   editing. If `osc vc` is unavailable for some reason, open the existing
   `.changes` file first and match its exact format by hand rather than
   guessing at one. Skip this only for a turn that makes no spec change (a
   pure investigation turn, or `action: "blocked"` with nothing edited).

41. **Obsolete build/test-runner shims in `setup.py`/`setup.cfg` — delete, do
   not package.** Some entries in `install_requires`/`setup_requires`/
   `tests_require` are leftovers from a much older Python ecosystem and are
   never worth adding as a BuildRequires:
   - `pytest-runner` in `setup_requires` — deprecated by its own author since
     2019; pytest has not needed a setuptools plugin to run for years. Delete
     it from the source rather than packaging it:
       sed -i '/pytest-runner/d' setup.py
     (real examples: `python-Arpeggio`, `python-abimap`, `python-CairoSVG`)
   - `nose` — unmaintained since ~2015, incompatible with current Python (it
     hits several convention 38 stdlib removals itself). A test suite that
     imports `nose` needs a source patch converting it to plain
     `unittest`/`pytest` idioms, not a `nose` BuildRequires — it will not
     build on current Python regardless:
       sed -i '/nose/d' setup.py
     (real examples: `python-kmatch`, `python-pyshould`, `python-pytimeparse`)
   - `argparse`, `typing` as plain (non-versioned-backport) entries in
     `install_requires` — both have been part of the stdlib since Python 2.7
     and 3.5 respectively; a real dependency on either at this point is
     always Python-2-compat cruft. Delete the line rather than adding a
     BuildRequires for either.
   Symptom that points here: `pip`/the build backend's dependency resolution
   fails, or the log shows a package trying to fetch one of these during
   `%build` (which cannot reach the network at all — convention 13's
   network-in-%build pattern) even though nothing in the package's own code
   imports it for anything real.

42. **Hardcoded/placeholder version strings need `%{version}` substitution.**
   Distinct from convention 22 (dynamic version *providers* like
   setuptools_scm needing a BuildRequires): here the package's own source
   already contains a literal, static version string that is simply wrong —
   stale from the last release, or an obvious placeholder the release
   process was supposed to template in and didn't for a source (repository-
   archive, convention 26) tarball. Symptom: the package builds and imports
   fine, but reports its own version as `0.0.0`, `0.0.0-dev`, `dev0`,
   `@VERSION@`, or a previous release's number when you'd expect
   `%{version}` — this can also make dependants that check
   `importlib.metadata.version()` at runtime, or the package's own version-
   gated code paths, behave incorrectly. Grep for the literal in `setup.py`/
   `pyproject.toml`/a `_version.py` and substitute in %prep:
     sed -i 's/__version__ = ".*"/__version__ = "%{version}"/' setup.py
     sed -i -e '/^version =/s/@VERSION@/%{version}/' pyproject.toml
   Find the exact existing string with Grep first — do not guess the quoting
   or variable name — and reuse this pattern instead: `%{version}` is a bare
   value; do not simply add %python_expand or macros around it.

43. **`setup.cfg`-based pytest config needs `[tool:pytest]`, not `[pytest]`.**
   pytest only recognizes a bare `[pytest]` section in `pytest.ini`/`tox.ini`;
   inside `setup.cfg` specifically, the same options must live under
   `[tool:pytest]` (singular "tool") or pytest silently ignores the whole
   section. `pyproject.toml` has its own separate form,
   `[tool.pytest.ini_options]`, already covered by convention 12 — this one
   is about `setup.cfg` only. Symptom: `testpaths`/`addopts`/
   `markers` configured in `setup.cfg`'s `[pytest]` section appear to do
   nothing — pytest collects 0 tests (exit code 5) or ignores an `addopts`
   you can plainly see in the file. Fix by renaming the section, not by
   reaching for `%pytest_arch <path>` first — that hides the real fix:
     sed -i 's/\[pytest\]/[tool:pytest]/' setup.cfg
   (two packages in the corpus made this exact rename but mistakenly wrote
   the non-existent `[tools:pytest]`, plural — verify you write `tool:pytest`,
   singular, since a typo here reproduces the same silent-ignore bug.)

44. **Two failure modes that are not a code bug in the package at all** —
   recognize these so you don't spend an attempt "fixing" something that was
   never broken:
   - **A test file/fixture is simply absent from the release tarball.**
     Some maintainers' `MANIFEST.in`/packaging config excludes test helper
     files from the sdist even though the test files that `import` them are
     included, so specific tests fail with an import/file-not-found error no
     patch can fix — the file does not exist anywhere in `source/`. Confirm
     with `find source -iname '<missing-file>*'` returning nothing.
     "Missing from this sdist" is not the same as "the project has none" —
     before deselecting, check whether the file/directory exists in the
     project's upstream repository at the matching tag/commit; if it does,
     fetch it as its own `SourceN` rather than losing that coverage:
       Source1: https://raw.githubusercontent.com/<org>/<repo>/<tag-or-commit>/tests/helpers.py
       (or, for a whole directory the project prepares as a tarball)
       Source1: %{name}-tests-%{version}.tar.gz
     and extract/copy it into place in %prep (`%autosetup -a1 -p1 -n ...` for
     a tarball Source1, or a plain `cp %{SOURCE1} tests/` for a single file)
     — pin to a commit/tag, not a moving branch ref (real examples:
     `python-pretend` for a single fetched test file, `python-pure-protobuf`
     for a whole fetched tests-tarball). Only fall back to deselecting
     (convention 11 rung i/ii) and saying so in your reasoning when no such
     upstream source exists either — do not report a `BuildRequires`, and do
     not spend an attempt trying to reconstruct the missing file from
     scratch. Real examples of the deselect fallback: `python-engineio`
     (`tests/async/files` missing, so `test_static_file_routing` and
     related tests are deselected), `python-simple-websocket` (`helpers.py`
     missing from the release tarball).
   - **An unrecognized trove classifier breaks the build backend's own
     metadata validation**, before your code or tests ever run. A
     `pyproject.toml` `classifiers` list containing a very recently added
     value (e.g. `Programming Language :: Python :: 3.14` on an older pinned
     `setuptools`) can make that backend's own metadata validation reject the
     whole build — the traceback comes from inside `setuptools`/the build
     backend, not from the package. Confirm the error is in metadata
     validation (not the package's code), then drop the offending line —
     dropping a classifier changes no runtime behavior, it is pure PyPI
     metadata:
       sed -i '/Programming Language :: Python :: 3.14/d' pyproject.toml
     (real examples: `python-pydantic`, `python-argon2-cffi`). Note in your
     reasoning that this is a build-backend-version workaround so a future
     attempt can drop it once the workaround is no longer needed.

45. **A Django-integration test suite needs `DJANGO_SETTINGS_MODULE` set
   before `%check`, not a BuildRequires fix.** Symptom:
   `django.core.exceptions.ImproperlyConfigured: settings are not configured`,
   even though `%{python_module Django}` is already a BuildRequires and
   installed correctly. Use the `django-test-settings` skill for the full
   treatment (finding the test tree's own settings module, exporting
   `DJANGO_SETTINGS_MODULE`, and when to gate the tests behind a `%bcond`
   instead).

46. **Strip a stray shebang from a library file that is meant to ship, don't
   delete the file.** A plain importable module under `%{python_sitelib}`
   can carry a `#!/usr/bin/env python`-style first line that upstream left
   in for standalone-run convenience even though the file is never meant to
   be executed directly once installed. rpmlint flags this as
   `non-executable-script`/`wrong-script-interpreter`, feeding convention
   25's badness budget — but the file is legitimate library content, so the
   fix is removing just the shebang line in `%prep`/`%build`, not convention
   23/24's "delete the whole file/directory" remedy (that's for content that
   genuinely should not ship at all):
     sed -i '1{/^#!/d}' path/to/module.py
   This recurs independently across many packages — `python-flickrapi`
   (`sed -i "1d" flickrapi/__init__.py`), `python-falcon` (several files
   under `falcon/bench/`, `falcon/cmd/`), `python-docutils`,
   `python-freetype-py`, `python-Durus`, `python-flake8-pyi` — always with a
   comment along the lines of "fix non-executable script". Confirm with
   `head -1` on the file the rpmlint warning names before patching; don't
   apply this blindly to every `.py` file in the tree.

47. **Set `%{optflags}` explicitly before building a C extension.** Recurs
   across compiled-extension packages that build via a bare
   `distutils`/`setuptools` `Extension()` rather than CMake (which already
   picks up distro flags through its own macros):
     export CFLAGS="%{optflags}"
   before `%pyproject_wheel`/`%build`, so the extension is compiled with
   distro hardening/optimization flags instead of setuptools' bare
   defaults (avoids rpmlint hardening-flag warnings feeding convention 25's
   badness budget). Real examples: `python-unicodedata2`, `python-ujson`,
   `python-uharfbuzz`. If a C-extension package's tests produce a
   wrong-but-plausible numeric or binary result (not a crash, not an
   ImportError) specifically under optimization, and the C source does
   pointer casts between incompatible types, check whether it relies on
   type-punning that strict-aliasing optimization miscompiles — `python-
   ujson` adds `-fno-strict-aliasing` to `CFLAGS` for exactly this reason.
   Try this before assuming the test itself is wrong or reaching for
   convention 11's skip ladder on a numeric-mismatch failure in a C
   extension's tests.

48. **Test-environment setup that isn't a missing BuildRequires**: two
   recurring cases where `%check` fails for a reason that has nothing to do
   with a missing package — fix the environment, not the dependency list.
   - **Locale-sensitive tests under OBS's default C/POSIX locale.** A test
     doing Unicode normalization, locale-aware sorting/formatting, or
     comparing output containing non-ASCII characters can fail purely
     because no locale is set — not because a package is missing (no
     `glibc-locale` BuildRequires needed either). Export a UTF-8 locale
     before `%check`:
       export LANG=en_US.UTF-8
       %pytest
     Extremely common — recurs across a large fraction of the corpus
     (`python-Whoosh`, `python-vcrpy`, `python-yamllint`,
     `python-CommonMark`, `cookiecutter`, and many more).
   - **A test calls out to `git` expecting an actual repository, not just
     the `git` binary.** This is different from convention 10's "missing
     VCS tool" case: `git-core` is already a BuildRequires and present, but
     a PyPI sdist has no `.git` directory, and some test suites (commonly
     ones that also use setuptools_scm/versioning at test time, not just
     build time) call `git describe`/`git rev-parse` and fail when there is
     no repo at all. Fix by synthesizing a minimal one in `%check` before
     running tests:
       git init
       git add .
       git commit -m "test commit" -q
       %pytest
     (real example: `python-poetry-core`, which also has to `unset
     SOURCE_DATE_EPOCH` right before its tests for the same underlying
     reason — its tests assert a specific fixed test-fixture date, and OBS's
     reproducible-build epoch environment variable overrides it. Check for
     this same symptom — a test comparing against a literal date/timestamp
     failing under OBS specifically — before assuming a real Y2038-style
     bug.)

── Rust / Cargo packages ─────────────────────────────────────────────────────

A standalone Rust package (binary/library built entirely with Cargo — a
Cargo.toml at the source root, no pyproject.toml at all) follows a completely
different set of conventions than everything above. Use the
`rust-cargo-packaging` skill for the full treatment (`cargo-packaging`
macros, vendored dependency tarballs, `%{cargo_build}`, manual binary
installation, shell-completion subpackages). This does NOT apply to a Python
package whose build backend happens to be maturin — that's convention 4's
`maturin-packaging` skill instead. A package with both Cargo and Python
components mixes the two skills' rules freely."#;

// ── public types ──────────────────────────────────────────────────────────────

/// Token counts and cost from one API call.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Pre-computed cost in USD from the CLI's own accounting.
    pub cli_cost_usd: Option<f64>,
}

impl Usage {
    pub fn cost_usd(&self) -> f64 {
        if let Some(usd) = self.cli_cost_usd {
            return usd;
        }
        (self.input_tokens as f64 * 5.0
            + self.output_tokens as f64 * 25.0
            + self.cache_creation_tokens as f64 * 6.25
            + self.cache_read_tokens as f64 * 0.50)
            / 1_000_000.0
    }

    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        match (self.cli_cost_usd.as_mut(), other.cli_cost_usd) {
            (Some(a), Some(b)) => *a += b,
            (None, Some(b)) => self.cli_cost_usd = Some(b),
            _ => {}
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run claude: {0}")]
    Launch(std::io::Error),

    #[error("claude exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("unexpected response: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Confidence the model reports about its fix.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Which task the agent should perform this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// Inspect the generated spec before the first build.
    Review,
    /// Fix the spec after a failed `osc build`.
    FixAfterBuild,
    /// Fix the spec after a failed `rpmspec --parse`.
    FixAfterValidation,
}

/// Per-turn tool-call and token budget.
#[derive(Debug, Clone)]
pub struct TurnBudget {
    pub max_tool_calls: usize,
    pub max_tokens: u64,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self { max_tool_calls: 15, max_tokens: 200_000 }
    }
}

/// Input to one agent turn.
pub struct TurnRequest<'a> {
    pub workspace: &'a std::path::Path,
    pub pkg_name: &'a str,
    pub version: &'a str,
    pub task: Task,
    pub attempt: usize,
    pub max_attempts: usize,
    pub spent_usd: f64,
    /// Total USD budget for the whole package, so the turn can be capped at
    /// whatever is left rather than being allowed to overrun it in one call.
    pub budget_usd: f64,
    pub hint: Option<&'a ExtractedLog>,
    pub notes: &'a str,
    pub budget: TurnBudget,
    /// The tarball is a repository archive rather than a PyPI sdist.
    pub from_repo_archive: bool,
    /// Sibling projects found next to the one being built, as
    /// `(directory, declared name)`. Non-empty only for a monorepo.
    pub subprojects: &'a [(String, String)],
    /// Packages OBS reported as unresolvable during the last build attempt.
    pub unresolvable_deps: &'a [String],
    /// Packages this run already tried to build, and failed to. These are the
    /// only ones it is right to work around rather than report again.
    pub failed_deps: &'a [String],
    /// Session ID from the previous turn, used to resume the claude session.
    pub session_id: Option<&'a str>,
    /// OBS checkout directory — when set, QUILT_PATCHES is pointed here so the
    /// agent can create patches that land directly in the OBS package directory.
    pub checkout_dir: Option<&'a std::path::Path>,
    /// Optional short note injected into the prompt just before the hint section.
    pub context: Option<&'a str>,
    /// Called in real-time as each tool-use block is parsed, with `(tool_name,
    /// input_summary)`. Used by the pipeline to stream AgentToolUse events
    /// while the turn is still running so the TUI stays alive.
    pub on_tool_use: Option<&'a (dyn Fn(&str, &str) + Send + Sync)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Edited,
    NoChange,
    Blocked { why: String },
}

/// One tool invocation the agent made during a turn.
pub struct ToolCall {
    pub name: String,
    /// The tool's input, JSON-encoded and truncated to a readable length.
    pub input: String,
}

pub struct TurnOutcome {
    pub action: Action,
    pub reasoning: String,
    pub evidence: Vec<String>,
    pub unresolvable_packages: Vec<String>,
    pub confidence: Confidence,
    pub usage: Usage,
    pub tool_calls: Vec<ToolCall>,
    pub prompt: String,
    pub output: String,
    /// The raw `result` payload from the CLI, before parsing.
    pub raw: String,
    pub session_id: Option<String>,
}

// ── turn ──────────────────────────────────────────────────────────────────────

/// Plugin bundling the conditional packaging skills (maturin, Rust/Cargo,
/// plain C/C++, monorepos, prebuilt wheels, systemd, i18n, -doc subpackages,
/// Django test settings) that CONVENTIONS only briefly points at instead of
/// spelling out in full — most packages never need any of them, so their
/// detailed content is loaded on demand via the `Skill` tool rather than
/// sent on every turn. Resolved at compile time relative to this crate, so
/// it only works when the binary runs from within its own source checkout
/// (the normal way this tool is used today).
const PACKAGING_SKILLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../packaging-skills");

const TURN_RESPONSE_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "action": {"type": "string", "enum": ["edited", "no_change", "blocked"]},
    "why": {"type": "string"},
    "reasoning": {"type": "string"},
    "evidence": {
      "type": "array",
      "items": {"type": "string"},
      "description": "Every file you Read or pattern you Grepped before editing, e.g. ['Read source/pyproject.toml', 'Grep for console_scripts in source/']. Must be non-empty when action is edited."
    },
    "confidence": {"type": "string", "enum": ["high", "medium", "low"]},
    "unresolvable_packages": {"type": "array", "items": {"type": "string"}}
  },
  "required": ["action", "reasoning", "evidence", "confidence"],
  "additionalProperties": false
}"#;

#[derive(Deserialize)]
struct TurnResponse {
    action: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    evidence: Vec<String>,
    reasoning: String,
    confidence: Confidence,
    #[serde(default)]
    unresolvable_packages: Vec<String>,
}

/// Run one agent turn. The agent has tool access to the workspace and edits
/// `spec` in place. The caller is responsible for validating and rolling back
/// on failure.
///
/// Uses `--output-format stream-json` so the agent's text and tool calls are
/// printed to stderr in real time as they arrive. The structured JSON response
/// is parsed from the final `result` event in the stream.
pub fn turn(req: TurnRequest<'_>) -> Result<TurnOutcome, Error> {
    let prompt = build_turn_prompt(&req);

    let allowed_tools = if req.checkout_dir.is_some() {
        "Read,Grep,Edit,Write,Skill,Bash(rpmspec *),Bash(quilt *),Bash(cd source && quilt *),Bash(osc status),Bash(osc vc *),Bash(ls *),Bash(rm *),Bash(zypper *),Bash(curl -s https://pypi.org/*),WebSearch"
    } else {
        "Read,Grep,Edit,Write,Skill,Bash(rpmspec *),Bash(osc status),Bash(ls *),Bash(rm *),Bash(zypper *),Bash(curl -s https://pypi.org/*),WebSearch"
    };

    // Escalate with the attempt number: the first 3 attempts go to the
    // previous-generation Sonnet (cheapest), the next 3 to Sonnet 5, and
    // everything from attempt 6 on — the packages the cheaper tiers already
    // failed on — to Opus 5.
    let model = if req.attempt <= 2 {
        "claude-sonnet-4-5"
    } else if req.attempt <= 5 {
        "claude-sonnet-5"
    } else {
        "claude-opus-5"
    };

    // Cap this turn at whatever is left of the package budget, so one runaway
    // turn cannot overshoot it — the caller only checks between turns.
    let remaining_usd = (req.budget_usd - req.spent_usd).max(0.01);

    let mut cmd = Command::new("claude");
    cmd.args([
        "--print",
        "--verbose",
        "--output-format",
        "stream-json",
        "--model",
        model,
        "--json-schema",
        TURN_RESPONSE_SCHEMA,
        "--add-dir",
        &req.workspace.to_string_lossy(),
        "--plugin-dir",
        PACKAGING_SKILLS_DIR,
        "--allowed-tools",
        allowed_tools,
        "--max-budget-usd",
        &format!("{remaining_usd:.4}"),
    ]);
    // The agent edits the spec through the checkout, not through the
    // workspace's `spec` symlink, so it needs access to both directories.
    if let Some(checkout) = req.checkout_dir {
        cmd.args(["--add-dir", &checkout.to_string_lossy()]);
    }
    if let Some(checkout) = req.checkout_dir {
        cmd.env("QUILT_PATCHES", checkout.as_os_str());
    }
    if let Some(id) = req.session_id {
        cmd.args(["--resume", id]);
    }
    cmd.args(["-p", &prompt]);

    let mut child = cmd
        .current_dir(req.workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(Error::Launch)?;

    // Collect stderr in a background thread so it doesn't block stdout reads.
    let stderr_handle = {
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let _ = std::io::BufReader::new(stderr).read_to_string(&mut buf);
            buf
        })
    };

    // Stream stdout line by line: display text/tool events and collect the
    // final `result` event which carries the structured JSON response.
    #[derive(Deserialize, Default)]
    struct ResultEvent {
        #[serde(default)]
        result: String,
        #[serde(default)]
        total_cost_usd: Option<f64>,
        #[serde(default)]
        usage: StreamUsage,
        #[serde(default)]
        num_turns: usize,
        #[serde(default)]
        session_id: Option<String>,
    }
    #[derive(Deserialize, Default)]
    struct StreamUsage {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    }

    let mut result_event = ResultEvent::default();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(child.stdout.take().unwrap());
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };
            match event.get("type").and_then(|t| t.as_str()) {
                Some("assistant") => {
                    // Text and tool_use arrive as content blocks of the
                    // assistant message — there is no top-level `tool_use`
                    // stream event to match on.
                    if let Some(blocks) = event
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in blocks {
                            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("(unnamed)");
                                let input = block
                                    .get("input")
                                    .map(|i| i.to_string())
                                    .unwrap_or_default();
                                let summary: String = input.chars().take(160).collect();
                                if let Some(cb) = req.on_tool_use {
                                    cb(name, &summary);
                                }
                                tool_calls.push(ToolCall {
                                    name: name.to_string(),
                                    input,
                                });
                            }
                        }
                    }
                }
                Some("result") => {
                    if let Ok(ev) = serde_json::from_value::<ResultEvent>(event) {
                        result_event = ev;
                    }
                }
                _ => {}
            }
        }
    }

    let stderr_content = stderr_handle.join().unwrap_or_default();
    let status = child.wait().map_err(Error::Io)?;
    if !status.success() {
        return Err(Error::Failed {
            code: status.code().unwrap_or(-1),
            stderr: stderr_content,
        });
    }

    let resp: TurnResponse = serde_json::from_str(result_event.result.trim())
        .map_err(|e| Error::Parse(format!("bad turn JSON: {e}\nraw: {}", result_event.result)))?;
    let _ = result_event.num_turns;

    let usage = Usage {
        input_tokens: result_event.usage.input_tokens,
        output_tokens: result_event.usage.output_tokens,
        cli_cost_usd: result_event.total_cost_usd,
        ..Usage::default()
    };

    let action = match resp.action.as_str() {
        "edited" => Action::Edited,
        "no_change" => Action::NoChange,
        _ => Action::Blocked { why: resp.why },
    };

    // Build minimal output summary for output.md
    let output = if tool_calls.is_empty() {
        format!("No tool calls made.\nAction: {:?}\n", action)
    } else {
        let tool_summary = tool_calls
            .iter()
            .map(|tc| format!("  • {}", tc.name))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Made {} tool call(s):\n{}\n\nAction: {:?}\n",
            tool_calls.len(),
            tool_summary,
            action
        )
    };

    Ok(TurnOutcome {
        action,
        reasoning: resp.reasoning,
        evidence: resp.evidence,
        unresolvable_packages: resp.unresolvable_packages,
        confidence: resp.confidence,
        usage,
        tool_calls,
        prompt,
        output,
        raw: result_event.result,
        session_id: result_event.session_id,
    })
}

// ── prompt construction ───────────────────────────────────────────────────────

fn build_turn_prompt(req: &TurnRequest<'_>) -> String {
    let task_desc = match req.task {
        Task::Review => {
            "Review the generated spec before the first build.\n\
             \n\
             Read `source/pyproject.toml` and `source/setup.cfg` (and `source/setup.py` \
             if those are absent) FIRST — almost every check below is answered there. \
             `source/` is a symlink to the unpacked tarball's top-level directory, so \
             use that prefix; `sdist/pyproject.toml` does not exist.\n\
             \n\
             The generator has already deduplicated BuildRequires, normalised the \
             version-constraint spelling, and set `%autosetup -n` from the tarball. \
             Do not spend the turn re-checking those.\n\
             \n\
             ## CRITICAL BUGS FIRST (fix immediately if found)\n\
             \n\
             0a. **Duplicated dist-info path**: The generator has a known bug where it \
                 sometimes creates `package-%{version}.dist-info/package-%{version}.dist-info/` \
                 (doubled path). Read the spec's %files section FIRST and check the dist-info \
                 entry. If it's duplicated, fix it immediately to the single path: \
                 `package-%{version}.dist-info/`\n\
             \n\
             0b. **Maturin package detection**: Grep `source/pyproject.toml` for \
                 `build-backend.*maturin`. If found, this is a Rust extension package and the \
                 generator's arch guess is WRONG. Load the `maturin-packaging` skill (see \
                 convention 4) for the full treatment — Source1 registry.tar.zst, \
                 cargo-packaging, ExclusiveArch, CARGO_* env vars, etc. — and apply ALL of it \
                 before checking anything else; these changes affect the entire spec structure.\n\
             \n\
             0c. **Source0 extension mismatch**: `ls sdist/` and compare the actual tarball's \
                 extension against the spec's `Source0:` line — the generator sometimes writes \
                 `.zip` when the real file is `.tar.gz` (or vice versa). This fails `%prep` with \
                 'No such file or directory' on the very first build. Fix `Source0:` to match \
                 the real filename now, before anything else depends on it.\n\
             \n\
             0d. **System tools wrapped in `%{python_module ...}`**: Grep the spec's \
                 BuildRequires for `%{python_module cmake}`, `%{python_module ninja}`, or the \
                 same for meson, patchelf, swig, make, gcc, g++, pkg-config, pkgconfig, rustc, \
                 cargo, autoconf, automake, libtool, bison, flex, gettext. These are system \
                 binaries invoked via subprocess, never `import`ed, so OBS has no \
                 `python<NNN>-<tool>` package to resolve and the build fails dependency \
                 resolution with an empty build log. Unwrap them to plain package names: \
                 `BuildRequires: cmake` / `BuildRequires: ninja`, etc.\n\
             \n\
             ## Main Review Checklist\n\
             \n\
             The generator switched to arch macros if it saw C/C++/Cython source — that's \
             a guess, so confirm it against convention 3 first. Then work through these in order:\n\
             \n\
             1. **The import name** the package installs — convention 16. This is the \
                single most common thing the generator gets wrong. Always verify from \
                `source/*.egg-info/top_level.txt` (definitive) or the source code. Never \
                derive it from the PyPI package name.\n\
             \n\
             2. **Console scripts**: Read `[project.scripts]` or `entry_points` in \
                source/pyproject.toml or source/setup.cfg. If ANY scripts are declared, \
                verify ALL python_alternative components are present (convention 18):\n\
                - %python_clone -a %{buildroot}%{_bindir}/<script> in %install (one per script)\n\
                - %python_expand %fdupes at end of %install\n\
                - Requires(post): update-alternatives\n\
                - Requires(postun): update-alternatives\n\
                - %post section with %python_install_alternative <script> (one per script)\n\
                - %postun section with %python_uninstall_alternative <script> (one per script)\n\
                - %python_alternative %{_bindir}/<script> in %files (one per script)\n\
                Missing ANY component will cause build failure.\n\
             \n\
             3. **Main runtime dependencies**: diff `[project.dependencies]` (pyproject.toml) \
                or `install_requires` (setup.cfg/setup.py) against the spec's Requires/ \
                BuildRequires. The generator sometimes copies test/optional dependencies \
                faithfully but drops the package's own main dependencies entirely — %check \
                then fails to even `import` the package. Add any missing ones as \
                `BuildRequires: %{python_module <name>}` (needed so %check can import them) \
                AND `Requires: %{python_module <name>}` (needed at install time). Do NOT copy \
                the name's casing verbatim from the dependency list — convention 2 explains \
                why that is frequently wrong (e.g. a lowercase `pillow` entry in \
                pyproject.toml still needs `%{python_module Pillow}`) and how to verify the \
                canonical spelling.\n\
             \n\
             4. **Test dependencies** from TWO sources:\n\
                a. DECLARED: `[project.optional-dependencies]` or `[dependency-groups]` \
                   (pyproject.toml) and `[options.extras_require]` (setup.cfg), keys like \
                   `test`, `tests`, `dev`, `testing`.\n\
                b. UNDECLARED: Grep the test files for imports and cross-reference against \
                   declared deps — many packages import test dependencies without declaring them.\n\
                Add BOTH declared and undeclared as `BuildRequires: %{python_module <name>}`, \
                and make sure pytest itself is there.\n\
             \n\
             5. **Pytest configuration** — `addopts`, `testpaths`, `fail_under`: \
                convention 12. Fix these now rather than after a failed build.\n\
             \n\
             6. **Build-backend extras**: dynamic version and metadata providers \
                (convention 22), and the C/C++ libraries an extension links against \
                (convention 3). For C extensions, confirm the -devel package name:\n\
                - pybind11 → usually pybind11-devel, but plain %{python_module pybind11} \
                  if setup.py gets the include path from pybind11.get_include() — see \
                  convention 3's pybind11 exception\n\
                - numpy → %{python_module numpy-devel} (for numpy/arrayobject.h)\n\
                - cmake, gcc-c++ always needed for CMakeLists.txt\n\
             \n\
             7. **%fdupes target must match package type** (common generator error):\n\
                - Arch packages: %python_expand %fdupes %{buildroot}/%{$python_sitearch}\n\
                - Noarch packages: %python_expand %fdupes %{buildroot}/%{$python_sitelib}\n\
             \n\
             8. **%pytest vs %pytest_arch** (critical for C extensions):\n\
                - Arch packages MUST use %pytest_arch in %check\n\
                - Noarch packages use %pytest\n\
                Using %pytest on an arch package will cause test failures.\n\
             \n\
             9. **License** if it is FIXME or not an SPDX expression (convention 5).\n\
             \n\
             10. **Sibling projects**, if the archive holds more than one — decide the \
                subpackage layout now, before the first build, because it changes \
                %build, %install and every %files section (convention 30).\n\
             \n\
             If a check does not apply, say so in one clause; do not invent work."
        }
        Task::FixAfterBuild => {
            "Fix the spec after a failed `osc build`.\n\
             \n\
             If the failure is in `%check`, prefer skipping the failing tests over \
             adding more dependencies — see convention 11. The hint below is a \
             heuristic extract of the failure; read build.log yourself whenever \
             the extract looks like it caught the wrong thing."
        }
        Task::FixAfterValidation => {
            "Fix the spec after a failed `rpmspec --parse`.\n\
             \n\
             The spec does not parse, so there is no build log — run \
             `rpmspec --parse spec` yourself to see the error, and again after \
             editing to confirm the fix. `rpmspec.txt` holds the stderr from the \
             failure that triggered this turn."
        }
    };

    // State the budget that is actually enforced — the turn is launched with
    // `--max-budget-usd`, and the run stops when the package budget is gone.
    let budget_line = match req.task {
        Task::Review => format!(
            "Pre-build review. ${remaining:.2} of model budget left for this package.",
            remaining = (req.budget_usd - req.spent_usd).max(0.0),
        ),
        _ => format!(
            "Fix attempt {attempt} of {max}. ${spent:.2} spent on this package so far, \
             ${remaining:.2} left. Investigate as much as you need within that.",
            attempt = req.attempt,
            max = req.max_attempts,
            spent = req.spent_usd,
            remaining = (req.budget_usd - req.spent_usd).max(0.0),
        ),
    };

    let ws = req.workspace;
    let mut map_lines = Vec::new();
    for (name, desc, always) in &[
        ("spec", "the spec file — see \"Editing the spec\" below", true),
        ("spec.generated", "pristine generated spec for reference", false),
        ("build.log", "full osc build log from the last attempt", false),
        ("rpmspec.txt", "rpmspec --parse stderr from the last failure", false),
        ("source/", "THE UNPACKED SOURCE — read pyproject.toml etc. under here", false),
        ("sdist/", "the extraction root; `source/` is the directory inside it", false),
        ("notes.md", "attempt journal — also inlined below", false),
    ] {
        let path = ws.join(name);
        if *always || path.exists() {
            map_lines.push(format!("  {name:<20} {desc}"));
        }
    }
    let workspace_map = map_lines.join("\n");

    // `spec` in the workspace is a symlink into the OBS checkout, and Edit
    // refuses to write through symlinks — hand over the real path up front
    // rather than letting the agent discover it by failing.
    let spec_section = match std::fs::read_link(ws.join("spec")) {
        Ok(target) => format!(
            "## Editing the spec\n\
             \n\
             `spec` in the workspace is a symlink and Edit cannot write through a \
             symlink. Read and Edit this path instead — it is the same file:\n\
             \x20 {}\n\
             \n",
            target.display(),
        ),
        Err(_) => String::new(),
    };

    let context_section = match req.context {
        Some(note) => format!("## Context\n\n{note}\n\n"),
        None => String::new(),
    };

    let hint_section = if let Some(hint) = req.hint {
        let mut s = String::from(
            "The following is a heuristic extract of the build log. \
             It may be incomplete or wrong. The full log is at build.log.\n\n",
        );
        if !hint.excerpt.is_empty() {
            s.push_str("Excerpt:\n```\n");
            s.push_str(&hint.excerpt);
            s.push_str("\n```\n");
        }
        if !hint.error_lines.is_empty() {
            s.push_str("Error lines:\n");
            for l in &hint.error_lines {
                s.push_str("  ");
                s.push_str(l);
                s.push('\n');
            }
        }
        s
    } else {
        String::new()
    };

    let source_note = if req.from_repo_archive {
        "\nNOTE: PyPI publishes this package as wheels only, so the tarball is an \
         archive of the upstream repository, not an sdist. Read convention 26 before \
         anything else — the project is quite likely in a subdirectory.\n\
         \n\
         Run `ls sdist/` FIRST and compare it against the spec's `%autosetup -n` \
         argument. Repository archives routinely extract to a directory named after \
         the whole repo (e.g. `cuda-python-13.4.2`), not after this specific package \
         (e.g. `cuda-bindings-13.4.2`), and the generator's guess is wrong almost every \
         time for this source type. A wrong `-n` fails %prep immediately with 'No such \
         file or directory' — fix it before anything else.\n"
    } else {
        ""
    };

    let subproject_section = if req.subprojects.is_empty() {
        String::new()
    } else {
        format!(
            "## Other projects in this archive\n\n\
             The source holds several projects side by side. `{primary}` is the one \
             this spec was generated for; these are all of them:\n{list}\n\n\
             Read convention 30 — the usual answer is one subpackage per project in \
             this single spec, not one project built and the rest ignored.\n\n",
            primary = req.pkg_name,
            list = req
                .subprojects
                .iter()
                .map(|(dir, name)| format!("  - {dir}/  (declares itself `{name}`)"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };

    let notes_section = if req.notes.is_empty() {
        String::new()
    } else {
        format!("## Prior attempt journal (notes.md)\n\n{}\n\n", req.notes)
    };

    let bullets = |deps: &[String]| {
        deps.iter()
            .map(|d| format!("  - {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let unresolvable_section = if req.unresolvable_deps.is_empty() {
        String::new()
    } else {
        format!(
            "## Missing BuildRequires\n\n\
             OBS could not find these RPM packages in any of its repositories:\n{}\n\n\
             They are not packaged yet. **Your goal is to get this package built by \
             packaging ALL missing dependencies.** Put their names in \
             `unresolvable_packages` in your response and the build system will build \
             them and retry this package — that is the normal path, and it applies \
             however large the dependency is. Leave their BuildRequires lines in the spec.\n\n\
             Do not stop at the first missing dependency — report ALL unresolvable \
             packages so they can be built in parallel. This iterative process continues \
             until all dependencies are satisfied and the final package successfully builds.\n\n\
             **CRITICAL - Verify before reporting python-* packages**:\n\
             1. **Check PyPI first**: Before reporting a python-* package as unresolvable, \
             verify it exists on PyPI: `curl -s https://pypi.org/pypi/<name>/json`. If you \
             get a 404, the package doesn't exist on PyPI.\n\
             2. **Check name variations**: PyPI package names use `-` while imports use `_`. \
             Try variations (e.g., `rich-toolkit` vs `rich_toolkit`).\n\
             3. **System tools are NOT Python packages**: Tools like cmake, ninja, pkg-config, \
             gcc, patchelf, etc. are ALWAYS system packages, never python-*. If you see missing \
             `python314-cmake`, `python314-patchelf`, etc., this is WRONG:\n\
             - WRONG: `BuildRequires: %{{python_module cmake}}`\n\
             - CORRECT: `BuildRequires: cmake`\n\
             - WRONG: `BuildRequires: %{{python_module patchelf}}`\n\
             - CORRECT: `BuildRequires: patchelf`\n\
             Common system tools: cmake, ninja, pkg-config, gcc, g++, make, automake, \
             autoconf, libtool, gettext, flex, bison, patchelf.\n\
             4. **A missing `python<NNN>-<dep>` is a flavor gap, not a new package**: \
             `%{{python_module <dep>}}` expands to one BuildRequires per active Python flavor \
             on this build target, and a newer flavor sometimes lacks a subpackage that older \
             flavors already have (e.g. `python314-cython` missing while `python312-cython` \
             builds fine). Do NOT put the flavor-prefixed name in `unresolvable_packages` — the \
             pipeline strips the `python<NNN>-` prefix before treating a name as buildable, so \
             reporting `python314-cython` just queues a redundant duplicate build of `cython`, \
             which already exists. Leave the BuildRequires as `%{{python_module <dep>}}` in the \
             spec; the gap closes on its own once that flavor catches up.\n\
             5. **Fix the spec**: If the spec incorrectly uses `%{{python_module <system-tool>}}`, \
             remove the macro and use the plain package name instead. Do NOT report these \
             as unresolvable.\n\n\
             Exception: if a package name in the spec is clearly wrong (e.g. wrong \
             capitalisation or an extra prefix), fix the name in the spec instead of \
             reporting it as unresolvable.\n\n",
            bullets(req.unresolvable_deps),
        )
    };

    // The evidence convention 28 asks for: these were attempted and did not
    // build, so working around them is justified and re-reporting them is not.
    let failed_section = if req.failed_deps.is_empty() {
        String::new()
    } else {
        format!(
            "## Dependencies that failed to build\n\n\
             This run already tried to build these and could not:\n{}\n\n\
             Do NOT put them in `unresolvable_packages` again — that is the retry \
             cycle convention 15 Step B exists to break. Work around them instead: \
             skip the tests that need them, or build without the feature that needs \
             them, and say in your reasoning which one you did and why.\n\n",
            bullets(req.failed_deps),
        )
    };

    format!(
        "You are fixing an openSUSE RPM spec for the package {pkg} {version}.\n\
         Task: {task}\n\
         \n\
         {budget}\n\
         \n\
         **GOAL**: Your ultimate objective is to get this package successfully built. \
         When you encounter missing dependencies, report ALL of them in \
         `unresolvable_packages` so they can be packaged. This process continues \
         recursively — each missing dependency will be packaged, which may reveal more \
         missing dependencies, until the entire dependency tree is satisfied and this \
         package builds successfully.\n\
         \n\
         {source_note}\
         ## Workspace\n\
         {map}\n\
         \n\
         {spec_section}\
         {conventions}\n\
         \n\
         CRITICAL: NEVER delete the spec file completely. You can edit it, but it must \
         remain present. Deleting it will break the build system.\n\
         \n\
         {subprojects}\
         {notes}\
         {unresolvable}\
         {failed}\
         {context}\
         {hint}\
         \n\
         ### Finding Missing C Headers and Libraries\n\
         \n\
         When a build fails with \"fatal error: <header.h>: No such file or directory\" or \
         linker errors about missing libraries:\n\
         \n\
         1. **Search for the providing package**: Use `zypper search --provides --match-exact \"/usr/include/<header.h>\"` \
         or `zypper what-provides '<library>.so*'` to find which package provides the missing file.\n\
         \n\
         2. **Add the -devel package**: C headers are typically in -devel packages. \
         For example, if openssl headers are missing, add `BuildRequires: openssl-devel`.\n\
         \n\
         3. **Common patterns**:\n\
         - `Python.h: No such file` → add `BuildRequires: %{{python_module devel}}`\n\
         - `pkg-config` errors → add `BuildRequires: pkg-config` or `pkgconfig(<name>)`\n\
         - Library linking errors → search for the library and add its -devel package\n\
         - CUDA headers/libraries → add `BuildRequires: cuda-devel` and `cuda-runtime`\n\
         \n\
         4. **Investigation tools**: Use `zypper search`, `zypper what-provides`, or check \
         the build log for hints about which library the build system is trying to find.\n\
         \n\
         If zypper finds nothing, this is convention 27's territory: report the package in \
         `unresolvable_packages` rather than working around it.\n\
         \n\
         ## Source Files and Cleanup\n\
         \n\
         Before finishing, check for untracked files and source availability:\n\
         \n\
         1. **Check untracked files**: Run `osc status` in the package directory to see \
         untracked files (marked with `?`).\n\
         2. **Handle each untracked file**:\n\
         - If it's a legitimate source file needed for the build, add it to the spec with \
         `Source<N>:` (where N is the next available number).\n\
         - If it's a generated file, build artifact, or not needed, delete it with `rm`.\n\
         - **IMPORTANT**: Avoid tarballs of different versions of the current package \
         (these are leftovers from previous attempts). Delete them unless they're \
         explicitly referenced in the spec.\n\
         3. **Verify all sources exist**: Read the spec and check that every `Source:` and \
         `Source<N>:` line points to a file that actually exists in the package directory. \
         Use `ls` to verify files exist. Missing sources will cause the build to fail.\n\
         4. **Examples of files to DELETE**:\n\
         - Old version tarballs: `{pkg}-1.2.3.tar.gz` when the current version is 1.2.4\n\
         - Build artifacts: `.pyc`, `__pycache__/`, `*.egg-info/`, `.pytest_cache/`\n\
         - Editor temporaries: `.swp`, `*~`, `.#*`\n\
         \n\
         ## Workflow\n\
         \n\
         1. **Investigate FIRST**: Use your tools to gather information BEFORE editing.\n\
         2. **Edit the spec**: Use Edit on the spec path given above (never return the \
         full spec as text). You MUST call the Edit tool BEFORE setting action to \
         \"edited\". Never describe a change in reasoning without having called Edit \
         first. If you identified a fix but did not apply it, set action to \
         \"no_change\" and explain why.\n\
         3. **Check sources and cleanup**: Verify all Source: and Source<N>: lines point \
         to files that exist. Run `osc status` and handle untracked files (see \"Source \
         Files and Cleanup\" above). Delete old version tarballs and build artifacts.\n\
         4. **Validate**: Run `rpmspec --parse <that same path>` to verify syntax. \
         Do this after your last edit, not only after the first one.\n\
         5. **Finish**: Return a JSON object with:\n\
         - \"action\": \"edited\", \"no_change\", or \"blocked\"\n\
         - \"why\": required when action is \"blocked\"\n\
         - \"reasoning\": one sentence summarising what you did\n\
         - \"evidence\": MUST list SPECIFIC files/patterns you Read or Grepped before editing.\n\
           REQUIRED format (each entry must name a specific file or pattern):\n\
           [\"Read source/pyproject.toml lines 1-50\",\n\
            \"Read source/my_package.egg-info/top_level.txt\",\n\
            \"Grep 'import' in source/tests/*.py\",\n\
            \"Grep 'console_scripts' in source/setup.cfg\"]\n\
           Generic entries like \"checked the source\" or \"verified files\" are NOT evidence.\n\
           Must be non-empty when action is \"edited\". This proves you investigated rather \n\
           than assumed. If you edited based on a specific file's content, that file MUST \n\
           appear in evidence.\n\
         - \"confidence\": \"high\", \"medium\", or \"low\"\n\
         - \"unresolvable_packages\": RPM names of packages that OBS cannot resolve and \
         must be built first. **IMPORTANT**: Before listing any python-* package here, \
         verify it exists on PyPI with `curl -s https://pypi.org/pypi/<name>/json`. Never \
         report system tools (cmake, ninja, pkg-config, gcc) as python-* packages — fix \
         the spec to use the plain package name instead.",
        pkg = req.pkg_name,
        version = req.version,
        task = task_desc,
        budget = budget_line,
        map = workspace_map,
        source_note = source_note,
        subprojects = subproject_section,
        spec_section = spec_section,
        conventions = CONVENTIONS,
        notes = notes_section,
        unresolvable = unresolvable_section,
        failed = failed_section,
        context = context_section,
        hint = hint_section,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conventions are cross-referenced by number in the prompt itself
    /// ("see convention 9"), so a duplicate or skipped number silently points
    /// the model at the wrong rule.
    #[test]
    fn conventions_are_numbered_consecutively_from_one() {
        let numbers: Vec<usize> = CONVENTIONS
            .lines()
            .filter_map(|l| {
                let (head, rest) = l.split_once(". ")?;
                if rest.starts_with(char::is_whitespace) {
                    return None;
                }
                head.parse::<usize>().ok()
            })
            .collect();
        assert!(!numbers.is_empty(), "no numbered conventions found");
        let expected: Vec<usize> = (1..=numbers.len()).collect();
        assert_eq!(numbers, expected, "convention numbering is not 1..n");
    }

    fn request<'a>(
        ws: &'a std::path::Path,
        subprojects: &'a [(String, String)],
    ) -> TurnRequest<'a> {
        TurnRequest {
            workspace: ws,
            pkg_name: "cuda-python",
            version: "13.0.2",
            task: Task::Review,
            attempt: 0,
            max_attempts: 10,
            spent_usd: 0.0,
            budget_usd: 2.0,
            hint: None,
            notes: "",
            budget: TurnBudget::default(),
            from_repo_archive: true,
            subprojects,
            unresolvable_deps: &[],
            failed_deps: &[],
            session_id: None,
            checkout_dir: None,
            context: None,
            on_tool_use: None,
        }
    }

    #[test]
    fn a_monorepos_sibling_projects_reach_the_prompt() {
        let ws = std::path::Path::new("/nonexistent-workspace");
        let subs = vec![
            ("cuda_bindings".to_string(), "cuda-bindings".to_string()),
            ("cuda_python".to_string(), "cuda-python".to_string()),
        ];
        let prompt = build_turn_prompt(&request(ws, &subs));

        assert!(prompt.contains("## Other projects in this archive"), "{prompt}");
        assert!(prompt.contains("cuda_bindings/  (declares itself `cuda-bindings`)"));
        assert!(prompt.contains("convention 30"));
        // And the repository-archive note, since this did not come from a sdist.
        assert!(prompt.contains("archive of the upstream repository"));
    }

    #[test]
    fn a_single_project_archive_says_nothing_about_subpackages() {
        let ws = std::path::Path::new("/nonexistent-workspace");
        let prompt = build_turn_prompt(&request(ws, &[]));
        assert!(!prompt.contains("## Other projects in this archive"));
    }

    /// Every `convention N` reference must name a convention that exists.
    #[test]
    fn convention_cross_references_resolve() {
        let count = CONVENTIONS
            .lines()
            .filter_map(|l| l.split_once(". ").and_then(|(h, _)| h.parse::<usize>().ok()))
            .max()
            .unwrap_or(0);
        for text in [CONVENTIONS] {
            for (i, _) in text.match_indices("convention ") {
                let n: usize = text[i + "convention ".len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .expect("convention reference must be numeric");
                assert!(n >= 1 && n <= count, "dangling reference to convention {n}");
            }
        }
    }
}
