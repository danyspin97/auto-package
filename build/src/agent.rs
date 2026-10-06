//! AI-assisted spec repair — tool-using turn loop only (REDESIGN §2.4).
//!
//! The only backend is the `claude` CLI (`--agent-backend cli`). Vertex AI and
//! direct-API backends are removed in M1 per REDESIGN §5.1.

use std::collections::BTreeMap;
use std::process::Command;

use serde::Deserialize;

use crate::log::ExtractedLog;

// ── subprocess environment scrubbing ──────────────────────────────────────────

/// Env vars the Claude subprocess is allowed to inherit from this process.
///
/// Added in response to the Oct 2026 external review of `build/src/agent.rs`:
///
/// > The Claude subprocess inherits the caller's environment and grants tool
/// > access including `rm`, `zypper` and `rpmspec`. There is no OS sandbox or
/// > credential separation implemented here. Prompt/tool restrictions alone
/// > are not that boundary.
///
/// A real OS sandbox / credential separation is out of scope for this change,
/// but the obvious credential-flow hazard — OBS publishing credentials,
/// cloud/forge push tokens etc. silently reaching a subprocess that holds
/// `Bash(rm *)`, `Bash(zypper *)` and `Bash(curl ...)` tool grants — is not.
/// Publishing (osc commit/push, GitHub tokens, cloud credentials) happens in
/// the pipeline's own subprocess calls, NOT in the agent's turn; the agent
/// legitimately needs none of them.
///
/// This list is intentionally short — anything not listed here is dropped,
/// including things the agent might arguably want (SSH_AUTH_SOCK, GPG_*,
/// NIX_PATH, …). Add new entries only after confirming the agent actually
/// needs them AND that forwarding them does not grant publish/network/auth
/// authority outside the local checkout.
const AGENT_ENV_ALLOWLIST: &[&str] = &[
    // Tool bootstrapping: many CLIs refuse to start without a HOME, and
    // `claude` itself writes session state under $HOME/.claude.
    "HOME",
    "USER",
    "LOGNAME",
    // Needed to locate `claude`, `osc`, `rpmspec`, `curl`, `quilt`, …
    "PATH",
    // Pretty output / terminal detection on the stderr side. Some tools only
    // emit color warnings when TERM is set.
    "TERM",
    "TERMINFO",
    // Locale — a missing LANG makes Python and gettext-wrapped tools default
    // to the C locale and corrupt non-ASCII filenames in spec files.
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    // Scratch and cache paths. If the user has moved these off $HOME (e.g.
    // onto a bigger disk), honoring their choice is cheaper than silently
    // thrashing / in a multi-package run — and claude's own session cache
    // lives under $XDG_CACHE_HOME.
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    // The ONLY credentials this list keeps: the Anthropic API key (and its
    // alternate spellings). Everything else — OBS, AWS, GCP, Azure, GitHub,
    // GitLab, netrc, GIT_ASKPASS — is dropped on the floor.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_API_KEY",
];

/// Case-sensitive suffix patterns that mark a credential-shaped env var.
///
/// The vetted ANTHROPIC_* / CLAUDE_* entries in `AGENT_ENV_ALLOWLIST` are
/// checked first and short-circuit out — this guard only applies to
/// everything else, including `extra_allow` entries from a (future)
/// `--pass-env` CLI flag. That way `--pass-env=MYSERVICE_TOKEN` would still
/// be dropped with a loud warning rather than silently forwarded.
const CREDENTIAL_SUFFIX_PATTERNS: &[&str] = &["_TOKEN", "_SECRET", "_PASSWORD", "_API_KEY"];

/// Build the environment map handed to the `claude` subprocess.
///
/// Written as a pure function over `(name, value)` pairs so the unit tests
/// can drive it with crafted inputs (OSC_PASSWORD, AWS_SECRET_ACCESS_KEY,
/// FOO_TOKEN, …) that we don't want in — and often don't have in — CI. The
/// production call site is a one-liner: `scrub_env_for_agent(std::env::vars()
/// .collect(), &[])`.
///
/// `extra_allow` is the extension point for a future `--pass-env=FOO,BAR` CLI
/// flag: entries whose names appear there are kept exactly as if they were in
/// `AGENT_ENV_ALLOWLIST`. The credential-suffix check still fires on them, so
/// `--pass-env=MYSERVICE_TOKEN` would still be dropped with a warning rather
/// than silently forwarded. Pass `&[]` when no extension is requested.
pub(crate) fn scrub_env_for_agent(
    vars: Vec<(String, String)>,
    extra_allow: &[String],
) -> BTreeMap<String, String> {
    let mut kept: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in vars {
        // Order matters: the explicit allow-list wins over the credential
        // suffix guard, because the Anthropic/Claude keys we DO want to keep
        // (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_API_KEY`)
        // obviously look credential-shaped and would otherwise be stripped.
        // Those names are vetted by hand in AGENT_ENV_ALLOWLIST.
        if AGENT_ENV_ALLOWLIST.contains(&name.as_str()) {
            kept.insert(name, value);
            continue;
        }
        // The suffix guard still fires on everything else — including
        // `extra_allow` entries from a (future) `--pass-env` flag — so that
        // forwarding a secret-shaped name is never silent.
        if matches_credential_pattern(&name) {
            eprintln!(
                "ap-build: scrubbing credential-shaped env var {name} from agent subprocess \
                 (publishing credentials and host secrets must not flow through — see the \
                 Oct 2026 build/src/agent.rs review)"
            );
            continue;
        }
        if extra_allow.iter().any(|a| a == &name) {
            kept.insert(name, value);
            continue;
        }
        // Strict allow-list: anything else is dropped silently. The common
        // case here is the hundreds of SHELL/EDITOR/XDG_SESSION_*/DBUS_*/…
        // variables an interactive shell exports, none of which the agent
        // needs; warning on each would drown out the credential warnings
        // above, which are the ones that actually matter.
    }
    kept
}

fn matches_credential_pattern(name: &str) -> bool {
    CREDENTIAL_SUFFIX_PATTERNS
        .iter()
        .any(|pat| name.ends_with(pat))
}

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

   **Cython packages: force regeneration by removing PKG-INFO.** When the
   package `BuildRequires: %{python_module Cython ...}` and ships `.pyx`
   files, Cython treats the presence of `PKG-INFO` in the source root as a
   signal that the sdist already contains pre-generated `.c` files and
   skips cythonization — meaning the pinned Cython version never actually
   runs. Add this to `%prep` after `%autosetup` to force regeneration:
     rm -f PKG-INFO
   Real example: `python-numpy`.

   For packages that ignore PKG-INFO and check for the generated files
   themselves (lxml's setup.py probes `src/lxml/lxml.etree.c` directly),
   the stronger form is to delete every generated artifact outright:
     find -name '*.c' -delete -print
     rm -f src/<pkg>/<pkg>.etree.h src/<pkg>/<pkg>.etree_api.h
   Only reach for the aggressive form when `rm -f PKG-INFO` alone doesn't
   trigger regeneration (build.log still reports the pinned Cython was
   not invoked). Real example: `python-lxml`.

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
   picking. `pybind11.get_cmake_dir()` counts as the CMake case too — it
   just returns the path to pybind11's own CMake config files for a
   manual `cmake -Dpybind11_DIR=...` invocation, so it still needs
   `%{python_module pybind11-devel}` for `find_package(pybind11 CONFIG)`
   to succeed (real example: `python-tokenspeed-triton`).

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

         **Recognize this shape immediately rather than spending an attempt
         on an environment fix first**: a test that re-invokes `python -I`
         (isolated mode — common for a `python -m <pkg>.cli ...`-style
         subprocess test) structurally cannot see `PYTHONPATH`, no matter how
         it is exported in the build environment — `-I` ignores it by
         design, not as a bug to work around. Go straight to deselecting
         these tests; do not try a `PYTHONPATH`/`sitecustomize` fix, since it
         cannot work (real example: `python-backports-zstd`'s
         `CommandLineTest` tests — the first attempt tried setting
         `PYTHONPATH` for exactly this failure and cost a whole extra
         attempt before deselecting them instead).

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
   - **The test *code* itself is incompatible with a newer pytest — this
     needs a patch, not a config change or a skip.** Not every pytest-version
     failure is fixable by an `addopts`/ini-level change: pytest 9.1 removed
     the ability for a `@pytest.fixture(scope='class')` method to take
     `self` — it must become a `@classmethod` taking `cls` instead — and a
     test file written for an older pytest fails with a confusing
     fixture-resolution error that looks like a code bug in the package, not
     a version mismatch. Before reaching for convention 11's skip ladder,
     check whether this is a known, already-fixed-elsewhere issue: search the
     upstream repo's issues/PRs, or another distro's packaging of the same
     project (Debian/Fedora), for a pytest-9.1-compatibility patch — this is
     common enough that one often already exists and just needs adapting,
     rather than hand-converting every affected fixture yourself. Apply it
     as a quilt patch (convention 14) tagged `PATCH-FIX-UPSTREAM` with the
     original commit/PR cited, the same as any other upstream fix — patching
     the test suite to pass is strictly better than skipping it, since it
     keeps real coverage instead of discarding it. (Real example:
     `python-pydantic-core` in openSUSE's own `devel:languages:python`
     carries exactly this shape of patch, sourced from Debian's packaging,
     for this exact pytest 9.1 fixture change.)

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

   **A bare `<name>.py` file is a different shape from a package directory —
   check which one you actually have before writing %files.** A project
   using `py_modules = ["<name>"]` (setup.py/setup.cfg) or an equivalent
   pyproject.toml `[tool.setuptools] py-modules` entry, rather than
   `packages = [...]`, installs a single top-level `.py` file with no
   `__init__.py` and no directory at all — the generator's default
   `package_name/` (with a trailing slash) then claims a directory that
   does not exist and fails with 'File not found'. Confirm by checking
   whether `source/`'s top-level entry for the import name (convention 16)
   is a file or a directory, and write the file-shaped entries instead:
     %{python_sitelib}/<name>.py
     %{python_sitelib}/__pycache__/<name>.cpython-*.pyc
   Do not forget the `__pycache__` entry — it is easy to remember the `.py`
   file and miss its compiled bytecode, which then shows up as a separate
   'Installed but unpackaged' error. Real examples: `python-six` (`six.py`),
   `python-nvidia-ml-py` (`pynvml.py`), `python-typing_extensions`
   (`typing_extensions.py`) — all three shipped as a bare file, not a
   directory, and the generator guessed directory for all three.

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

   A PEP 561 stub-only subdirectory (`<pkg>-stubs/`, containing only `.pyi`
   files and no `__init__.py`) needs its own `%dir` entry for the directory
   itself — globbing the files inside it (`<pkg>-stubs/*.pyi`) claims the
   files but not the directory, and rpm reports the directory itself as "not
   owned by a package" even though every file in it is claimed. Declare both:
     %dir %{python_sitearch}/<pkg>-stubs/
     %{python_sitearch}/<pkg>-stubs/*.pyi
   (real example: `python-duckdb`'s `_duckdb-stubs/`).

   A compiled extension's `.so` filename is easy to glob wrong by splitting
   the CPython tag and platform tag across two separate wildcard segments
   (`_mod.cpython-*-*.so`) — this can fail to match the real filename
   (`_mod.cpython-313-x86_64-linux-gnu.so`) even though it looks like it
   should. Prefer one wildcard spanning the whole variable suffix instead:
     %{python_sitearch}/_mod.cpython-*.so
   (real example: `python-duckdb`'s `_duckdb.cpython-*-*.so` → fixed to
   `_duckdb.cpython-*.so`).

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

   **`libalternatives` variant (Tumbleweed / `suse_version > 1500`).** On
   newer openSUSE, the `alts`/`libalternatives` mechanism is preferred over
   `update-alternatives`; gate the two with a `%bcond`. The `%python_clone`/
   `%python_alternative` macros are unchanged in both modes:
     %if 0%{?suse_version} > 1500
     %bcond_without libalternatives
     %else
     %bcond_with libalternatives
     %endif
     ...
     %if %{with libalternatives}
     BuildRequires:  alts
     Requires:       alts
     %else
     Requires(post):   update-alternatives
     Requires(postun): update-alternatives
     %endif
     ...
     %pre
     %python_libalternatives_reset_alternative <script>
   And in `%check`, when per-flavor tests shell out to the binary itself,
   stage symlinks under `build/flavorbin/` so the tests find each flavor's
   copy:
     %python_flavored_alternatives
     %if %{with libalternatives}
     %{python_expand #
     for b in <script1> <script2>; do
       [ -e build/flavorbin/$b ] || ln -s %{buildroot}%{_bindir}/$b-%{$python_bin_suffix} build/flavorbin/$b
     done
     }
     %endif
   Real example: `python-numpy` for `f2py` and `numpy-config`.

   Two script-grouping forms also exist and are easy to miss:
   - **Multiple scripts under one libalternatives group** —
     `%python_group_libalternatives <script1> <script2> ...` in `%install`
     bundles separate binaries into one alternatives group (so admin
     switches them atomically). Real example: `python-Sphinx` groups
     `sphinx-apidoc sphinx-autogen sphinx-build sphinx-quickstart`.
   - **Aliased scripts in one install/uninstall call** — when upstream
     ships two names for the SAME binary (not two different scripts),
     list both in one `%python_install_alternative` call and uninstall
     only the master:
       %post
       %python_install_alternative pytest py.test
       %postun
       %python_uninstall_alternative pytest
     Real example: `python-pytest` for the `pytest`/`py.test` pair.
   - **Man pages via alternatives too.** When each script has a
     corresponding man page the install should pair them. Clone the man
     page per flavor alongside the binary, then list the man pages in
     the same `%python_install_alternative` call AFTER the binary names
     and declare each `%python_alternative %{_mandir}/man1/<name>.1`
     entry in `%files`:
       %python_clone -a %{buildroot}%{_bindir}/<script>
       %python_clone -a %{buildroot}%{_mandir}/man1/<script>.1
       ...
       %post
       %python_install_alternative <script1> <script2> <script1>.1 <script2>.1
       %files
       %python_alternative %{_bindir}/<script1>
       %python_alternative %{_mandir}/man1/<script1>.1%{?ext_man}
     Real example: `python-Twisted` ships 8 binaries + 7 man pages this
     way under one alternatives group.

19. RPM macros use single '%', never '%%'. For example: %check, %install,
   %{version}, %{python_module foo}. Only use %% when you need a literal
   percent character in the spec file output (which is rare).

20. Python macros: ONLY use macros documented in the openSUSE wiki page
   "Packaging/Python" (https://en.opensuse.org/openSUSE:Packaging_Python),
   OR one this prompt's conventions/skills name explicitly (e.g.
   `%python_find_lang` in the `gettext-i18n-packaging` skill) — that wiki
   page is not a complete list of every real python-rpm-macros macro. Do NOT
   invent a macro beyond those two sources. If a macro you want is in
   neither, confirm it actually exists with `rpm -E %<macro>` (a real macro
   expands to something other than its own literal name) or
   `rpmspec --parse` before using it — do not guess from the name alone.

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
   - Multiple comma-separated constraints are not supported in one line
     — but two escape hatches exist for an actually-necessary bounded
     range (upstream documents an incompatibility, or you've seen the
     newer version break this specific build). Pick either:
     a. **Two separate BuildRequires lines** on the same package name —
        RPM resolves both as the intersection:
          BuildRequires: %{python_module pytest >= 7.0.0}
          BuildRequires: %{python_module pytest < 10}
        Real example: `python-SQLAlchemy`.
     b. **RPM4 boolean `with` inside one `%{python_module …}` argument**,
        referencing the flavor-aware `%python-<name>` form on the right
        side so each flavor's own package is bound:
          BuildRequires: %{python_module charset-normalizer >= 2.0 with %python-charset-normalizer < 4}
        Real example: `python-aiohttp`.
     Form (b) is more compact when the upper bound is a hard upstream
     cap for every flavor; form (a) is clearer when the two bounds have
     different reasons for existing. If the upper bound is NOT actually
     necessary, drop it per the next bullet instead.

     The RPM4 boolean syntax inside `%{python_module …}` also supports
     the `if` keyword for environment-markers-style conditional deps:
       BuildRequires: %{python_module importlib_resources >= 5 if %python-base < 3.10}
     Reads as "require importlib_resources on flavors whose base Python
     is < 3.10" — a direct equivalent of pyproject.toml's
     `importlib_resources; python_version < '3.10'`. Real example:
     `python-notebook`.
   - A constraint that is ONLY an upper bound (`< X`, no `>=` to keep) usually
     cannot be translated at all — drop it and use the bare name instead.
     `[build-system] requires` sometimes pins `setuptools<70.0.0` for an old
     build backend, but a rolling-release distro ships exactly one current
     version of `setuptools`; once that version passes 70, `%{python_module
     setuptools < 70.0.0}` can never resolve — OBS will report it unresolvable
     forever, not because the package is missing but because no installable
     version satisfies the constraint (if this surfaces later as an
     unresolvable BuildRequires instead of being caught here at
     spec-authoring time, the "Missing BuildRequires" section's item 6 below
     covers the same diagnosis).
       WRONG: BuildRequires: %{python_module setuptools < 70.0.0}
       RIGHT: BuildRequires: %{python_module setuptools}
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

   **If the installed package carries a `dist-info/RECORD` file, delete
   the corresponding RECORD lines too.** `RECORD` is pip's manifest of
   every file the wheel installed; downstream tooling (`pip check`,
   introspection via `importlib.metadata.files()`) compares it against
   the filesystem and reports deleted entries as broken. The one-liner:
     sed -i -e '/<name>-<ext>,/d' %{buildroot}%{$python_sitearch}/<pkg>-%{version}.dist-info/RECORD
   For a batch delete (e.g. the C sources after a Cython compile — real
   example `python-pandas`), match on the shared extension:
     find %{buildroot}%{$python_sitearch}/<pkg>/_libs -name '*.[ch]' -delete
     sed -i -e '/\.[ch],/d' %{buildroot}%{$python_sitearch}/<pkg>-%{version}.dist-info/RECORD

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
   the whole job — size and complexity do not matter.

   This applies to ALL dependency types without exception:
   - **Python packages**: torch, jax, transformers, pydantic — report them ALL
   - **C/C++ libraries**: report the upstream project name, NOT the
     `BuildRequires` string. Strip a leading `lib` and a trailing `-devel`/
     `-dev` before adding to `unresolvable_packages` — `BuildRequires:
     libtrantor-devel` is reported as `trantor`, `BuildRequires: libfoo-dev`
     as `foo`. The pipeline uses that name to search PyPI and then GitHub for
     the source repo; the openSUSE devel-package wrapping is a distro
     naming scheme upstream repos do not use, so reporting the wrapped
     spelling makes the lookup miss its own project and permanently mark the
     dep unbuildable (see "Dependencies that can never be built here" below
     — a dep landing there under a `lib…-devel` spelling is almost always
     this bug, not a real unbuildable dep; re-report it under the stripped
     name).
   - **System tools**: cmake, ninja, specialized build tools — report if missing
   - **CUDA libraries**: cuda-devel, cuda-runtime, CUDA Python packages
   - **ANY other dependency**: regardless of type, size, or complexity

   Do not treat a big or awkward package as out of reach, and do not disable
   a feature or drop a test suite merely because its dependency is not in
   OBS yet.

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
   3. Also add CUDA to `unresolvable_packages` if OBS reports it as
      unresolvable — report it regardless, there is no downside to reporting
      a dependency that turns out to already be satisfied. But know what to
      expect: the build system resolves a missing name from PyPI first, then
      falls back to a discoverable public source repository (the same
      fallback a wheel-only PyPI release uses) — `cuda-devel`/`cuda-runtime`
      specifically are the closed-source CUDA Toolkit compiler/runtime
      itself, with neither a PyPI package nor any public source anywhere, so
      this specific pair can only ever resolve if the target OBS project's
      own repository configuration already provides it (e.g. a path to
      NVIDIA's official repo, or a project maintainer's own import) — no
      amount of re-reporting manufactures a package that has no buildable
      source. This is unlike every CUDA *Python* ecosystem library below,
      which the build system CAN and does build automatically once reported.
   4. NEVER skip packages that require CUDA libraries — report the need even
      when you expect `cuda-devel`/`cuda-runtime` to stay unresolved; that is
      still correct information for whoever is watching the project, and
      costs nothing.

   CUDA Python ecosystem libraries are all open source and available on GitHub.
   When encountering any of these as dependencies, report them for packaging:
   cuda-python (cuda.core, cuda.bindings, cuda.pathfinder), cuda-compute,
   numba-cuda-mlir, cuda-tile, nvmath-python (nvmath.bindings), nvshmem4py,
   and similar NVIDIA CUDA Python libraries. Unlike `cuda-devel`/
   `cuda-runtime` above, these publish real source (PyPI sdist or a public
   GitHub repository) and the build system resolves and builds them the same
   way it does any other missing dependency — report them with full
   confidence, not just for completeness.

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

   A sibling shape pins the exact *version* of a named dependency (rather
   than resolving a file to a package name): when a Python package embeds
   code generated against another package's internals (classic case is
   anything using `cffi`'s out-of-line API — the generated bindings depend
   on the exact cffi version they were compiled with), bind runtime cffi to
   the exact build-time cffi:
     Requires:       python-cffi = %(rpm -q --whatprovides python-cffi --qf "%%{version}")
   Note the doubled `%%` — the `%(...)` shell escape is expanded by rpm
   first, so the format string's own `%` must be escaped. Real example:
   `python-cryptography`.

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

   **Special case — a PyPI package that just redistributes a system tool:**
   `ninja` exists on PyPI purely as a portable, pip-installable copy of the
   `ninja` build tool openSUSE already ships as its own system package of
   the same name — build backends like scikit-build-core/meson-python
   depend on it only to shell out to the `ninja` executable, not to
   `import ninja`. If you see `ninja` as a dependency — this project's own
   `requires_dist`, another project's missing-BuildRequires list, or
   anywhere else — do not put it in `unresolvable_packages` and do not
   write `%{python_module ninja}`; write plain `Requires: ninja` /
   `BuildRequires: ninja` instead. Building a `python-ninja` RPM from the
   wheel would only duplicate the system package that already provides the
   same binary. (This is the opposite of the dlopen clarification above,
   which is about dropping a dependency that was never real — here the
   dependency is real, it just already has a home under its plain system
   name.) Before applying this, confirm the actual consumer shells out to
   `ninja` rather than doing a literal `import ninja` in its own code —
   spot-checking one such case is what established this rule; if a future
   dependency genuinely imports the PyPI package's Python module, it needs
   the real `python-ninja`, and this substitution does not apply to it.

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

   Exception 4 — THIS package (not a dependency) cannot be compiled in this
   environment at all: the source is real and public, but `%build` needs a
   proprietary vendor compiler/SDK that has no distro package and no
   open-source substitute — e.g. NVIDIA's `nvcc` to compile `.cu` device
   code (`nvidia-nccl-cu13`, `nvidia-nvshmem-cu13`, CUDA-CCCL and similar all
   hit this: real GitHub source, but the actual GPU kernels cannot be
   compiled without NVIDIA's own toolchain). This is a property of the build
   environment, the same category as Exception 2, not a bug in the spec —
   do not spend attempts rewriting CMake flags or BuildRequires for it.
   Set action to `"use_upstream_wheel"` with `why` naming the exact missing
   compiler/SDK and quoting the build.log line that proves `%build` needs it
   (e.g. `find_package(CUDAToolkit REQUIRED)` failing, or a literal `nvcc:
   command not found`). The pipeline independently checks that PyPI actually
   publishes a wheel for this version before switching — it does nothing
   silently wrong if you are mistaken, but don't invoke this reflexively:
   if the failure is a missing BuildRequires, a wrong CMake path, or
   anything convention 13's offline-build guidance already covers, fix that
   instead. Once the pipeline swaps in the wheel skeleton, the next turn
   sees a fresh spec shaped like Exception 3's — load the
   `prebuilt-wheel-packaging` skill to finish it.

   This exception also covers Review discovering that the checked-out
   source is simply the wrong artifact for this specific PyPI release — a
   `find_source_repo_candidates` guess can be a real, existing repository
   (so it passed verification) while still being the wrong slice of it for
   this exact package (a monorepo's unrelated sibling, or the full project
   when this release is only a data/binary shim from one small part of it).
   If the checked-out tree's actual structure doesn't match what this
   release's own PyPI metadata (`Summary`, `Description`, what little
   `RECORD`/wheel filename tells you if one exists) says it should contain,
   that mismatch is itself the evidence — cite what you checked and what
   didn't line up, and use `use_upstream_wheel` the same way.

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

48. **Test-environment setup that isn't a missing BuildRequires**: four
   recurring cases where `%check` fails for a reason that has nothing to do
   with a missing package — fix the environment, not the dependency list.
   - **`%ctest` can silently drop a custom environment variable the test
     binary needs.** Unlike `%pytest`, `%ctest` does not reliably propagate
     an `export`ed variable through to the actual test invocation — a CTest
     suite that calls back into the just-built Python extension and needs
     `PYTHONPATH` pointing at the buildroot's installed module can fail with
     no trace in the build log of the export ever reaching the process, even
     though the `export` line itself ran. Run `ctest` directly with the
     variable set inline instead of relying on the macro to carry it:
       PYTHONPATH=%{buildroot}%{$python_sitearch} ctest --output-on-failure
     (real example: `triton`, where `%ctest` lost `PYTHONPATH` entirely —
     setting it inline on a direct `ctest` invocation fixed it).
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
   - **Process-introspection tests fail under OBS's build-worker sandbox.**
     A test that shells out to `ps` (or reads `/proc/<pid>/...` for another
     process) to confirm something about a process it just spawned — a
     changed process title, an argv/cmdline check — can fail even though the
     feature under test works correctly, because the build runs inside a PID
     namespace where child processes are not visible the way `ps`/`/proc`
     expect from outside it. This is an OBS sandboxing limitation, not a
     missing package (`procps`/`ps` itself is present and works) and not a
     bug in the package being tested. Deselect the specific tests rather
     than chasing a BuildRequires or patch (convention 11 rung i/ii), noting
     in your reasoning that the failure is sandbox visibility, not
     functionality (real example: `python-setproctitle`, 8 of 25 tests
     deselected this way while the other 17 — which check the in-process
     state directly rather than shelling out to `ps` — pass normally).
   - **Accumulate arch-specific deselects into a `test_failok` variable
     and run them as a best-effort second pass**, instead of `rm`-ing the
     files outright (which forfeits the chance to see them pass on another
     arch). Collect the deselect expressions arch-by-arch, then run them
     with `||:` after the main pass so a failure in the "known flaky"
     bucket does not fail the build:
       test_failok=""
       %ifarch s390x
       test_failok+=" or test_truncate_f32"
       %endif
       %ifarch %ix86 %arm
       test_failok+=" or test_mem_policy"
       %endif
       %pytest_arch -k "not (test_broken ${test_failok})"
       [ -n "$test_failok" ] && %pytest_arch -k "${test_failok:4}" ||:
     The `${test_failok:4}` strips the leading ` or ` so the second
     `-k` expression is well-formed. Real example: `python-numpy`.
   - **Source-tree shadowing when `%pytest_arch --pyargs <pkg>` is run
     from the extracted source root.** With `--pyargs`, pytest resolves
     the import name against `sys.path` — but the current directory is on
     `sys.path`, so the uncompiled `source/<pkg>/` directory is found
     before the installed buildroot copy. The symptom is a confusing
     "cannot import compiled extension" / circular-import error that
     looks like a missing BuildRequires. Rename the source directory
     before invoking pytest so only the installed copy is importable:
       mv <pkg> <pkg>.dont-import-me
       %pytest_arch --pyargs <pkg> -k "not ($donttest)"
     Only reach for this when the test command is `--pyargs <pkg>` or
     equivalent and the failure signature is import-time, not test-time.
     Real example: `python-scipy` renames its `scipy/` tree to
     `scipy.dont-import-me` right before `%pytest_arch --pyargs scipy`.

49. **Per-CPython-version-locked upstream wheels that exactly cover this
   project's flavors** (extends convention 28 Exception 4): sometimes a
   package's source is real and public but genuinely cannot be built here —
   Exception 4's proprietary-toolchain case, or a package-specific blocker
   like a custom pinned compiler fork the distro toolchain can't provide
   (e.g. Triton's `CMakeLists.txt` requires an exact untagged
   `triton-lang/llvm-project` commit distro LLVM does not build) — AND
   PyPI's only wheels for that release are each locked to one specific
   CPython ABI (`cp313-cp313-...`, `cp314-cp314-...`), with no `-none-any`
   and no `abi3` tag covering every flavor this project builds.

   The automatic `"use_upstream_wheel"` action (Exception 4) cannot handle
   this shape: it calls `pick_wheel_from_json`, which deliberately REFUSES
   every cp-locked wheel rather than guess one — `wheel_is_flavor_agnostic`
   in `ecosystem/src/pypi.rs` names Triton as the exact reason for that
   refusal — so the turn would fail with "no usable upstream wheel either"
   instead of switching. Do it by hand instead, as an ordinary `"edited"`
   action:
   a. Find which Python flavors this project actually builds:
      `rpm --eval '%{pythons}'` (e.g. `python313 python314`).
   b. Check the release's PyPI JSON
      (`https://pypi.org/pypi/<name>/<version>/json`) for a `bdist_wheel`
      matching EACH flavor's ABI tag (`cp313-cp313-manylinux..._<arch>.whl`
      for `python313`, and so on) for the target `%{arch}`. If any enabled
      flavor has no matching wheel, this technique doesn't fully apply —
      report the gap rather than guessing one.
   c. Delete `%build` entirely (there is nothing to compile) and add one
      `SourceN:` per flavor, each pointing at that flavor's own wheel URL.
   d. In `%install`, call each flavor's OWN install macro by its real,
      hardcoded name — not the shared multi-flavor `%pyproject_install`,
      which runs the identical argument for every flavor and so cannot pick
      a different wheel per flavor:
        %install
        %python313_pyproject_install %{SOURCE0}
        %python314_pyproject_install %{SOURCE1}
        %python_clone -a %{buildroot}%{_bindir}/<console-script>
        %python_expand %fdupes %{buildroot}%{$python_sitearch}
      (`%python<NNN>_pyproject_install` already runs `%python<NNN>_compile`
      and `%python<NNN>_fix_shebang` — the same macro the shared
      multi-flavor loop calls internally, just invoked once per flavor with
      a different argument instead of the same one N times.)
   e. Everything else — License from `source/*.dist-info/`,
      `Root-Is-Purelib` deciding arch vs. noarch, `%check` needing a real
      import/file check rather than a blind `%pytest_arch` if the actual
      test suite needs a GPU (Exception 2) — follows the
      `prebuilt-wheel-packaging` skill exactly as for a single wheel; load
      it for the multi-flavor worked example and finish the spec from
      there.

50. **OOM / disk-full build failures are a worker-sizing problem, not a
   spec bug.** A `%build` or `%check` killed by the OOM reaper, a linker
   that aborts partway through, or a "No space left on device" error on a
   package known to need gigabytes of RAM/disk to compile (numpy, scipy,
   pandas, torch, cutlass, llvm, chromium-class) is NOT fixed by trimming
   dependencies or passing `-j1`. Add a sibling `_constraints` file next to
   the spec so OBS routes the job to a bigger worker:

     <constraints>
       <hardware>
         <physicalmemory><size unit="G">8</size></physicalmemory>
         <disk><size unit="G">9</size></disk>
       </hardware>
     </constraints>

   Pick numbers from the actual failure (how much RAM/disk it was using
   when it died, rounded up a bit). The file is checked in alongside
   `_multibuild`/`_service`; no spec edit needed. Real example: `python-
   numpy` ships `_constraints` with 8G RAM / 9G disk for its C/Fortran
   compile step. If the failure signature is clear OOM or ENOSPC on a
   package that just needs more room, add this before chasing phantom
   dependency changes.

   **Complementary pattern — cap `%jobs` dynamically to fit current
   worker memory** instead of (or in addition to) demanding a bigger
   worker. When the OOM happens inside `pytest-xdist` or `make -j`
   whose per-worker footprint is known, `BuildRequires: memory-
   constraints` and call `%limit_build -m <MB-per-job>` once in `%check`
   or `%build`:
     BuildRequires: memory-constraints
     ...
     %check
     %limit_build -m 3972
     %pytest_arch -n %{jobs} ...
   The macro divides available RAM by the per-job number and sets
   `%jobs` accordingly, so the build still parallelizes but never
   overcommits. Use when a single job comfortably fits the default
   worker but N parallel jobs OOM. Real example: `python-pandas` sets
   `-m 3972` for its test collection. For a build that OOMs even at
   `-j1`, the `_constraints` file above is still the right fix.

51. **`meson-python` / `scikit-build-core` backend arguments go through
   `PIP_CONFIG_SETTINGS`, not direct meson/cmake invocations.** When
   `build-backend = "mesonpy"` or `build-backend = "scikit_build_core.build"`
   is in pyproject.toml and the package needs a non-default build flag (an
   arch-specific CPU baseline, a feature toggle, a disable for an
   unavailable optional dep), `%pyproject_wheel` cannot be told directly —
   it runs pip, which only forwards config settings to the PEP 517 backend
   through this env var. Set it in `%build` before `%pyproject_wheel`:

     %ifarch x86_64 %{ix86}
     export PIP_CONFIG_SETTINGS="setup-args=-Dcpu-baseline=none"
     %endif
     %pyproject_wheel

   The key is the backend's own name (`setup-args` for meson-python,
   `cmake.define.<VAR>` / `cmake.args` for scikit-build-core — check the
   backend's docs for the exact spelling). Multiple settings can be
   space-separated inside the quotes. Trying to add a bare `-Dfoo=bar` to
   `%meson_build` is wrong — that macro is not invoked for a PEP-517
   build. Real example: `python-numpy` passes `-Dcpu-baseline=none` on x86
   this way.

52. **PyPI package renames / forks need `Obsoletes` + `Provides` so the old
   name still resolves.** When this package is a fork or renamed
   continuation of an older PyPI project — the fork took over the
   namespace and `pip install <old-name>` now redirects to it, or the
   project literally renamed itself — downstream specs that still say
   `BuildRequires: python-<old-name>` must keep resolving. Pair every
   stale name under the one new package:
     Obsoletes:  python-<old-name> < %{version}
     Provides:   python-<old-name> = %{version}
   Give one `Obsoletes`/`Provides` pair per old name. Confirm the old
   name with PyPI's `/pypi/<old-name>/json` returning either 404 or a
   redirect/notice pointing at the new project before adding it — do not
   invent historical names from the package's own description text.

   Three shapes count as "old name" worth pairing:
   - **Case-only rename**: PyPI accepted a capitalization change (`django`
     → `Django`) and the old distro spec was in the other form. Pair
     `python-django` under the new `python-Django`.
   - **Fork or project rename**: `PIL` → `Pillow`, `imaging` → `Pillow`.
     One pair per prior name — `python-Pillow` has two.
   - **Separate project folded in**: a once-independent package now lives
     inside this one (e.g. `python-South`'s migrations tool was absorbed
     into Django's built-in `django.db.migrations`). Pair the absorbed
     project's distro name too so installing the new umbrella uninstalls
     the dead one — `python-Django` carries `Provides: python-South =
     %{version}` + `Obsoletes: python-South < %{version}` for exactly
     this reason.

   **Non-python-prefixed name → gate the pair to one flavor so it
   doesn't duplicate.** When the old name has no `python-` prefix at all
   (bare `mypy`, `pytest`, `jupyter-notebook`), one of the Python
   flavors needs to carry the `Provides`/`Obsoletes`, not every flavor
   (otherwise RPM reports duplicate Provides across the subpackages).
   Pin them to the primary flavor:
     %if "%{python_flavor}" == "python3" || "%{?python_provides}" == "python3"
     Provides:  <bare-name> = %{version}
     Obsoletes: <bare-name> < %{version}
     %endif
   Real example: `python-mypy` gates `Provides: mypy = %{version}` to
   the primary `python3` flavor.

53. **Optional heavy component → its own subpackage via `%exclude` from
   the main `%files` plus a `%files` block for the split.** When
   installing a Python package drops both the lightweight core and a
   heavy optional component into the same site-packages tree (a GUI
   binding like `PIL/ImageTk`, a plugin folder a casual consumer never
   loads), carve the optional piece into its own subpackage so the main
   package stays small. Keep `%install` unchanged (upstream drops both
   sets of files in one pass), split only at `%package`/`%files` time:

     %package tk
     Summary:  <main> - Tcl/Tk Module
     Requires: %{name} = %{version}
     Requires: python-tk
     %description tk
     ...

     %files %{python_files}
     %{python_sitearch}/<pkg>
     %exclude %{python_sitearch}/<pkg>/<ImageTk>*
     %exclude %{python_sitearch}/<pkg>/<_imagingtk>*
     %pycache_only %exclude %{python_sitearch}/<pkg>/__pycache__/<ImageTk>.*

     %files %{python_files tk}
     %{python_sitearch}/<pkg>/<ImageTk>*
     %{python_sitearch}/<pkg>/<_imagingtk>*
     %pycache_only %{python_sitearch}/<pkg>/__pycache__/<ImageTk>.*

   Three rules this makes concrete:
   - `%exclude` the split-off files from the main package AND list them
     in the subpackage. Omitting the `%exclude` leaves them owned twice;
     omitting the subpackage listing drops them from the build.
   - The compiled bytecode (`__pycache__/<Name>.*.pyc`) is a separate
     file from the `.py` and needs its own `%pycache_only %exclude` /
     `%pycache_only` entry alongside each code file — easy to forget and
     surfaces as a separate 'Installed (but unpackaged)' error.
   - The subpackage's `%files` line uses `%{python_files <name>}`, not
     a hand-rolled `-n python%{python_version_nodots}-<name>`.
   Real example: `python-Pillow` splits `ImageTk`/`_imagingtk` into
   `python-Pillow-tk`.

   **Python C-extension headers follow the same shape as a C library's
   `-devel`.** A compiled Python extension that exposes a C API ships
   `.h` headers inside its sitearch tree (`%{python_sitearch}/<pkg>/*.h`,
   sometimes also `includes/*.h`); other Python C extensions that extend
   this API `#include` them at build time. Split them into a `-devel`
   subpackage with the same `%exclude` from main + list in subpackage
   pattern:
     %exclude %{python_sitearch}/<pkg>/*.h
     %exclude %{python_sitearch}/<pkg>/includes/*.h
     ...
     %files %{python_files devel}
     %{python_sitearch}/<pkg>/*.h
     %{python_sitearch}/<pkg>/includes/*.h
   Real example: `python-lxml` ships `lxml.etree.h` /
   `lxml.etree_api.h` for extensions building against lxml's C API.

54. **Metapackage subpackage: pulls in a cluster of runtime deps, ships no
   files of its own.** For a feature that lives entirely in *other*
   packages — a LaTeX rendering mode that needs 15 `texlive-*` runtime
   packages, a GUI backend that only needs a toolkit's own binding, a
   plugin set that just has to be present — define a subpackage whose
   whole `%files` block is just the `%license` line, with all the real
   work done in `Requires:` lines and (if the base package is arch)
   `BuildArch: noarch`:

     %package latex
     Summary:        Allow rendering latex in %{name}
     Requires:       %{name} = %{version}
     Requires:       texlive-dvipng
     Requires:       texlive-latex
     Requires:       tex(helvet.sty)
     ...
     BuildArch:      noarch

     %files %{python_files latex}
     %license LICENSE/

   Installing `python-matplotlib-latex` then pulls the whole LaTeX chain
   as a side effect, without the base package growing a heavy hard
   `Requires` chain for a feature most users don't need. Keep the
   `%license` line even though the subpackage ships no code — an empty
   `%files` is a rpmlint warning. Real example: `python-matplotlib-latex`
   has 20+ `texlive-*` and `tex(…)` runtime `Requires` with a
   license-only `%files`.

55. **Two-pass pytest: xdist-safe tests in parallel, xdist-hostile tests
   serially.** When some tests fail only under `pytest-xdist`'s parallel
   worker scheduling — a GUI backend test that assumes it owns the
   display, a cwd-sensitive test, a test that depends on a specific
   process ordering — do NOT drop `-n auto` for the whole suite (losing
   8x+ wall-clock for a handful of flaky tests). Split them with a
   `no_xdist` accumulator and run two passes:

     no_xdist="test_backend or test_span_selector_animated_artists_callback"
     %{python_expand
     $python -m pytest --pyargs <pkg>.tests \
       -n auto \
       -k "not (${no_xdist} ${skip_tests})"
     $python -m pytest --pyargs <pkg>.tests \
       -k "(${no_xdist}) and not (${skip_tests:4})"
     }

   The first pass runs the bulk in parallel skipping both the no-xdist
   bucket and the usual deselect list. The second pass runs ONLY the
   no-xdist bucket without `-n auto`, still minus the deselect list. The
   `${skip_tests:4}` strips the leading ` or ` so the second `-k`'s
   expression is syntactically valid (same trim as convention 48's
   `test_failok` pattern). Real example: `python-matplotlib`.

56. **Publisher-signed checksums file (no direct tarball signature) → GPG
   the file, then verify each listed hash.** Some upstreams don't sign
   the release tarball directly — they publish and sign a single
   `<name>-<version>.checksum.txt` listing MD5/SHA1/SHA256 for every
   release artifact. Verifying the source then has two steps: GPG the
   checksums file against the maintainer keyring, then verify the
   tarball's hash from the file. Wire this up as three Sources plus a
   `%prep` prelude:

     Source:   <tarball-url>
     Source1:  <project>/<name>-%{version}.checksum.txt
     Source2:  %{name}.keyring
     BuildRequires: gpg2
     ...
     %prep
     gpg --import %{SOURCE2}
     gpg --verify %{SOURCE1}
     echo "`grep -e '^[0-9a-f]\{32\}  <name>-%{version}.tar.gz' %{SOURCE1} | cut -c1-32`  %{SOURCE0}" | md5sum  -c
     echo "`grep -e '^[0-9a-f]\{40\}  <name>-%{version}.tar.gz' %{SOURCE1} | cut -c1-40`  %{SOURCE0}" | sha1sum -c
     echo "`grep -e '^[0-9a-f]\{64\}  <name>-%{version}.tar.gz' %{SOURCE1} | cut -c1-64`  %{SOURCE0}" | sha256sum -c
     %autosetup -p1 -n <name>-%{version}

   One `grep | cut | <hash>sum -c` line per hash the file lists. The
   `.keyring` file is the same per-package `<Name>.keyring` as the
   plain-c-cpp "source signature verification" entry — reuse it. A single
   failing hash aborts `%prep`, which is the point. Do NOT substitute a
   plain `sha256sum -c` against the whole file — the file has multiple
   non-SHA256 lines that would be rejected. Real example:
   `python-Django`, which pairs `Source: django-%{version}.tar.gz` with
   `Source1: Django-%{version}.checksum.txt` and runs all three hash
   checks in `%prep`.

57. **Test suite needs the compiled extension in-tree, not just in the
   buildroot → run `make inplace` after `%pyproject_wheel`.** Some C-
   extension packages' test suites import the extension from a path
   relative to the source tree (not from `%{buildroot}%{python_sitearch}`)
   and skip or fail if the `.so` is only in the installed location. The
   pattern is a one-liner in `%build` right after the wheel builds:
     %build
     export CFLAGS="%{optflags}"
     %pyproject_wheel
     # Test suite imports the compiled extension in-place.
     %python_expand PYTHON=$python make inplace
   Then `%check` works against the source tree directly (`%python_exec
   test.py`, or `%pytest` with `cd src/<pkg>` first) without PYTHONPATH
   gymnastics. Reach for this only when the test suite documents an
   `inplace` build target or a first attempt without it fails with
   "cannot import compiled extension" from a path inside the source
   tree. Real example: `python-lxml`.

58. **Vendored third-party libraries → their LICENSE files must ride
   along in `%license`.** Build backends that need their own
   dependencies at bootstrap time (poetry-core, hatchling, pip itself)
   ship them vendored under `_vendor/`, `_vendored/`, `vendor/`, or
   `bundled/`. Each vendored library's own LICENSE/COPYING is a legal
   requirement to redistribute — missing them is an rpmlint error and a
   compliance issue. Collect them in `%prep` into a sibling directory,
   then list the whole set with `%license`:

     %prep
     %autosetup -p1 -n <pkg>-%{version}
     mkdir vendoredlicenses
     cp -p src/<pkg>/_vendor/lark/LICENSE          vendoredlicenses/lark.LICENSE
     cp -p src/<pkg>/_vendor/packaging/LICENSE     vendoredlicenses/packaging.LICENSE
     cp -p src/<pkg>/_vendor/packaging/LICENSE.APACHE vendoredlicenses/packaging.LICENSE.APACHE
     cp -p src/<pkg>/_vendor/tomli/LICENSE         vendoredlicenses/tomli.LICENSE
     ...
     %files
     %license LICENSE vendoredlicenses/*

   Enumerate every subdirectory under `_vendor/` — `ls
   source/src/<pkg>/_vendor/` and copy one line per LICENSE file it
   finds, including multi-license sets like
   `packaging/LICENSE{,.APACHE,.BSD}`. Do NOT substitute `%doc
   _vendor/*/LICENSE` — rpm needs the files out from under the Python
   source tree for the `%license` tagging to apply. Real example:
   `python-poetry-core` collects LICENSE files from 4 vendored libs
   (lark, fastjsonschema, packaging, tomli).

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

/// Workflow/header/cleanup boilerplate that, like [`CONVENTIONS`], is
/// identical on every turn regardless of package, task, or attempt — kept as
/// a separate constant (rather than folded into `CONVENTIONS` itself) only
/// because its numbered lists would otherwise collide with
/// `conventions_are_numbered_consecutively_from_one`'s check that the real
/// conventions number 1..n with no gaps or repeats. Combined with
/// `CONVENTIONS` in [`static_system_prompt`].
const STATIC_WORKFLOW: &str = r#"
CRITICAL: NEVER delete the spec file completely. You can edit it, but it must
remain present. Deleting it will break the build system.

### Finding Missing C Headers and Libraries

When a build fails with "fatal error: <header.h>: No such file or directory" or
linker errors about missing libraries:

1. **Search for the providing package**: Use `zypper search --provides --match-exact "/usr/include/<header.h>"`
or `zypper what-provides '<library>.so*'` to find which package provides the missing file.

2. **Add the -devel package**: C headers are typically in -devel packages.
For example, if openssl headers are missing, add `BuildRequires: openssl-devel`.

3. **Common patterns**:
- `Python.h: No such file` → add `BuildRequires: %{python_module devel}`
- `pkg-config` errors → add `BuildRequires: pkg-config` or `pkgconfig(<name>)`
- Library linking errors → search for the library and add its -devel package
- CUDA headers/libraries → add `BuildRequires: cuda-devel` and `cuda-runtime`

4. **Investigation tools**: Use `zypper search`, `zypper what-provides`, or check
the build log for hints about which library the build system is trying to find.

If zypper finds nothing, this is convention 27's territory: report the package in
`unresolvable_packages` rather than working around it.

## Source Files and Cleanup

Before finishing, check for untracked files and source availability:

1. **Check untracked files**: Run `osc status` in the package directory to see
untracked files (marked with `?`).
2. **Handle each untracked file**:
- If it's a legitimate source file needed for the build, add it to the spec with
`Source<N>:` (where N is the next available number).
- If it's a generated file, build artifact, or not needed, delete it with `rm`.
- **IMPORTANT**: Avoid tarballs of different versions of the current package
(these are leftovers from previous attempts). Delete them unless they're
explicitly referenced in the spec.
3. **Verify all sources exist**: Read the spec and check that every `Source:` and
`Source<N>:` line points to a file that actually exists in the package directory.
Use `ls` to verify files exist. Missing sources will cause the build to fail.
4. **Examples of files to DELETE**:
- Old version tarballs: `<pkg>-1.2.3.tar.gz` when the current version is 1.2.4
- Build artifacts: `.pyc`, `__pycache__/`, `*.egg-info/`, `.pytest_cache/`
- Editor temporaries: `.swp`, `*~`, `.#*`

## Workflow

1. **Investigate FIRST**: Use your tools to gather information BEFORE editing.
2. **Edit the spec**: Use Edit on the spec path given above (never return the
full spec as text). You MUST call the Edit tool BEFORE setting action to
"edited". Never describe a change in reasoning without having called Edit
first. If you identified a fix but did not apply it, set action to
"no_change" and explain why.
3. **Check sources and cleanup**: see "Source Files and Cleanup" above —
verify sources exist, handle untracked files, delete stale artifacts.
4. **Validate**: Run `rpmspec --parse <that same path>` to verify syntax.
Do this after your last edit, not only after the first one.
5. **Finish**: Return a JSON object with:
- "action": "edited", "no_change", "blocked", or "use_upstream_wheel"
- "why": required when action is "blocked" or "use_upstream_wheel" — for the
latter, name the exact missing compiler/SDK and quote the build.log line proving it
(convention 28 Exception 4)
- "reasoning": one sentence summarising what you did
- "evidence": MUST list SPECIFIC files/patterns you Read or Grepped before editing.
  REQUIRED format (each entry must name a specific file or pattern):
  ["Read source/pyproject.toml lines 1-50",
   "Read source/my_package.egg-info/top_level.txt",
   "Grep 'import' in source/tests/*.py",
   "Grep 'console_scripts' in source/setup.cfg"]
  Generic entries like "checked the source" or "verified files" are NOT evidence.
  Must be non-empty when action is "edited". This proves you investigated rather
  than assumed. If you edited based on a specific file's content, that file MUST
  appear in evidence.
- "confidence": "high", "medium", or "low"
- "unresolvable_packages": RPM names of packages that OBS cannot resolve and
must be built first. **IMPORTANT**: Before listing any python-* package here,
verify it exists on PyPI with `curl -s https://pypi.org/pypi/<name>/json`. Never
report system tools (cmake, ninja, pkg-config, gcc) as python-* packages — fix
the spec to use the plain package name instead."#;

/// Everything sent to `claude --append-system-prompt`: [`CONVENTIONS`] plus
/// [`STATIC_WORKFLOW`], neither of which varies by package, task, or attempt.
///
/// This used to be interpolated straight into the per-turn user prompt
/// (`-p`) alongside genuinely dynamic content — package name, workspace
/// paths, notes.md, build failure hints. Since Anthropic's prompt cache
/// only hits on a byte-identical prefix, mixing ~140KB of unchanging
/// instructions into a string that's different on every single call meant
/// it could never be served from cache, on any turn, for any package. As a
/// separate system-prompt append it becomes a stable prefix shared by every
/// turn across every package in a run — this is the entire point of moving
/// it here, not a stylistic preference.
fn static_system_prompt() -> &'static str {
    // `turn()` calls this once per agent turn — potentially hundreds of times
    // over a single `run_package` build+fix loop — and the ~140KB result
    // never changes within the process, so compute it once and hand out the
    // same allocation instead of re-formatting and recopying it every call.
    static PROMPT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PROMPT.get_or_init(|| format!("{CONVENTIONS}\n\n{STATIC_WORKFLOW}"))
}

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
    Failed {
        code: i32,
        stderr: String,
        stdout: String,
        prompt: String,
    },

    #[error("unexpected response: {message}")]
    Parse {
        message: String,
        stdout: String,
        prompt: String,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The prompt sent for the turn this error aborted, when one had already
    /// been built — `None` only for failures that precede that point
    /// (`Launch`, a stream-read `Io` error), which have nothing turn-specific
    /// to show.
    pub fn prompt(&self) -> Option<&str> {
        match self {
            Error::Failed { prompt, .. } | Error::Parse { prompt, .. } => Some(prompt),
            Error::Launch(_) | Error::Io(_) => None,
        }
    }

    /// The claude CLI's raw stdout transcript up to the point of failure, when
    /// captured — the single most useful artifact for diagnosing a crash or a
    /// malformed response, since it is otherwise discarded the moment this
    /// process exits (see `turn`'s `stdout_raw` doc comment).
    pub fn stdout(&self) -> Option<&str> {
        match self {
            Error::Failed { stdout, .. } | Error::Parse { stdout, .. } => Some(stdout),
            Error::Launch(_) | Error::Io(_) => None,
        }
    }

    /// The claude CLI's captured stderr, when any was produced.
    pub fn stderr(&self) -> Option<&str> {
        match self {
            Error::Failed { stderr, .. } => Some(stderr),
            Error::Parse { .. } | Error::Launch(_) | Error::Io(_) => None,
        }
    }
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
        Self {
            max_tool_calls: 15,
            max_tokens: 200_000,
        }
    }
}

/// Input to one agent turn.
pub struct TurnRequest<'a> {
    pub workspace: &'a std::path::Path,
    pub pkg_name: &'a str,
    pub version: &'a str,
    /// Whether this package is a Python project (per the `python-` RPM-name
    /// prefix set by `Ecosystem::rpm_name`). Controls which Review checklist
    /// the agent gets: a plain-C/C++ or Rust package has no pyproject.toml,
    /// no `%{python_module ...}` macros, no `%python_subpackages`, so handing
    /// it the Python-centric checklist (import name, console scripts, pytest
    /// config, %fdupes sitelib vs. sitearch) is a wall of irrelevant work —
    /// a real drogon run spent a whole Review turn concluding "no_change"
    /// against a 10-item Python checklist.
    pub is_python: bool,
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
    /// The raw `nothing provides <pkg> <op> <version>` / `(got version
    /// <installed>)` lines behind `unresolvable_deps`, verbatim — `..._deps`
    /// itself keeps only the bare package name (needed to queue it for
    /// building), which reads identically whether the package is genuinely
    /// missing or, as in the `setuptools<70.0.0` case that prompted this,
    /// already present at a version this spec's own upper-bound constraint
    /// rules out. Empty when OBS gave no version detail to show.
    pub unresolvable_detail: &'a [String],
    /// Packages this run already tried to build, and failed to. These are the
    /// only ones it is right to work around rather than report again.
    pub failed_deps: &'a [String],
    /// Packages confirmed to have no PyPI project and no discoverable public
    /// source repository at all — this pipeline can never build them, no
    /// matter how many times they're reported (almost always a proprietary
    /// vendor SDK/compiler like NVIDIA's `cuda-devel`). See convention 28
    /// Exception 4.
    pub unbuildable_deps: &'a [String],
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
    pub on_tool_use: Option<&'a ToolUseCallback<'a>>,
}

/// Streaming per-tool-use callback — clippy's `type_complexity` floor
/// hits the inline `dyn Fn(&str, &str) + Send + Sync` on `on_tool_use`.
pub type ToolUseCallback<'a> = dyn Fn(&str, &str) + Send + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Edited,
    NoChange,
    Blocked {
        why: String,
    },
    /// The package's own source cannot be built in this environment (e.g. it
    /// requires a proprietary compiler/toolchain OBS cannot provide, per
    /// convention 28 Exception 3's "environment" case) and a prebuilt wheel
    /// exists on PyPI for this exact version. `why` is the agent's cited
    /// evidence — the pipeline still independently verifies a wheel exists
    /// before acting on this.
    UseUpstreamWheel {
        why: String,
    },
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
    "action": {"type": "string", "enum": ["edited", "no_change", "blocked", "use_upstream_wheel"]},
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

/// Pick a usable `TurnResponse` out of a finished turn's output.
///
/// The normal source is the `result` event's `result` field. When the CLI
/// aborted mid-turn (budget exhausted, time limit, SIGPIPE) that field is
/// empty, but the model may still have called `StructuredOutput` earlier in
/// the stream — its `input` has the same shape as the final response, so
/// using it salvages a valid fix the pipeline would otherwise discard as
/// "claude exited 1".
///
/// Last-resort: Haiku occasionally describes the structured response in prose
/// instead of calling the tool ("I've already called the StructuredOutput tool
/// at the end of my response. The structured output includes: `**action**:
/// no_change`, `**unresolvable_packages**: trantor, jsoncpp, ...`"). Parse the
/// markdown-style `**key**: value` lines so the pipeline can still queue the
/// reported deps — otherwise the whole turn (and the four C++ libs drogon
/// listed) is discarded for a formatting mistake.
fn pick_turn_response(result_payload: &str, tool_calls: &[ToolCall]) -> Option<TurnResponse> {
    if let Ok(r) = serde_json::from_str::<TurnResponse>(result_payload.trim()) {
        return Some(r);
    }
    if let Some(r) = tool_calls
        .iter()
        .rev()
        .find(|t| t.name == "StructuredOutput")
        .and_then(|t| serde_json::from_str::<TurnResponse>(&t.input).ok())
    {
        return Some(r);
    }
    parse_turn_response_from_prose(result_payload)
}

/// Extract a [`TurnResponse`] from markdown prose of the form
/// ```text
/// - **action**: `no_change`
/// - **reasoning**: The spec is correct ...
/// - **evidence**: a, b, c
/// - **confidence**: `high`
/// - **unresolvable_packages**: `trantor`, `jsoncpp`
/// ```
///
/// Only used after both JSON parse paths fail. Treats every field as optional
/// at the parse level — missing required fields fall back to safe defaults
/// (`action: "no_change"`, empty evidence, `confidence: "low"`) so the
/// pipeline can at least act on whatever list the model gave; the caller
/// still rejects a response that has no `action` marker at all.
fn parse_turn_response_from_prose(text: &str) -> Option<TurnResponse> {
    fn find_field(text: &str, key: &str) -> Option<String> {
        let needle = format!("**{key}**:");
        let start = text.find(&needle)? + needle.len();
        let rest = &text[start..];
        let end = rest.find('\n').unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
    fn strip_wrapping(s: &str) -> String {
        s.trim().trim_matches('`').trim().to_string()
    }
    fn split_list(s: &str) -> Vec<String> {
        s.split(',')
            .map(strip_wrapping)
            .filter(|v| !v.is_empty())
            .collect()
    }

    // Require at least `action` — without it there is no shape at all, and
    // treating a random text reply as a `no_change` would mask real bugs.
    let action = strip_wrapping(&find_field(text, "action")?);
    let reasoning =
        find_field(text, "reasoning").unwrap_or_else(|| text.trim().chars().take(500).collect());
    let evidence = find_field(text, "evidence")
        .map(|s| split_list(&s))
        .unwrap_or_default();
    let confidence_raw = find_field(text, "confidence")
        .map(|s| strip_wrapping(&s))
        .unwrap_or_else(|| "low".to_string());
    let confidence = match confidence_raw.to_lowercase().as_str() {
        "high" => Confidence::High,
        "medium" => Confidence::Medium,
        _ => Confidence::Low,
    };
    let unresolvable_packages = find_field(text, "unresolvable_packages")
        .map(|s| split_list(&s))
        .unwrap_or_default();

    Some(TurnResponse {
        action,
        why: String::new(),
        evidence,
        reasoning,
        confidence,
        unresolvable_packages,
    })
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

    // Escalate with the attempt number: the first 2 attempts go to Haiku 4.5
    // (cheapest), the next 3 to Sonnet 4.5, and everything from attempt 6 on —
    // the packages the cheaper tiers already failed on — to Sonnet 5.
    let model = if req.attempt <= 2 {
        "claude-haiku-4-5-20251001"
    } else if req.attempt <= 5 {
        "claude-sonnet-4-5"
    } else {
        "claude-sonnet-5"
    };

    // Cap this turn at whatever is left of the package budget, so one runaway
    // turn cannot overshoot it — the caller only checks between turns.
    let remaining_usd = (req.budget_usd - req.spent_usd).max(0.01);

    // Sent via `--append-system-prompt` instead of folded into `prompt`
    // below — see `static_system_prompt`'s doc comment for why that's what
    // actually lets this ~140KB of unchanging instructions be served from
    // Anthropic's prompt cache instead of re-billed as fresh input on every
    // single turn, for every package.
    let system_prompt = static_system_prompt();

    let mut cmd = Command::new("claude");
    // Scrub the inherited environment before applying any `.env(...)` call
    // below: `env_clear` drops the parent env, `envs(scrubbed)` adds back
    // only the allow-listed entries. See `scrub_env_for_agent` for the
    // rationale (Oct 2026 review of this file). Any extra `cmd.env(...)`
    // after this point — e.g. QUILT_PATCHES below — is additive and NOT
    // credential-shaped, so it is fine to set after the clear.
    //
    // No `--pass-env` CLI flag yet; wire the extension through by threading
    // a `&[String]` down from the caller when one is added.
    let scrubbed_env = scrub_env_for_agent(std::env::vars().collect(), &[]);
    cmd.env_clear().envs(&scrubbed_env);
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
        "--append-system-prompt",
        system_prompt,
        // The default system prompt's per-machine sections (cwd, env info,
        // git status) otherwise differ across packages' checkout
        // directories and would break cache-prefix reuse between them even
        // with a shared `--append-system-prompt`.
        "--exclude-dynamic-system-prompt-sections",
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
            // `read_to_string` would fail outright on any invalid UTF-8 byte
            // and discard everything read — plausible for a crash that dumps
            // a native backtrace or truncates mid-codepoint. Reading raw
            // bytes and converting lossily means a crash-stderr.log is never
            // silently empty just because one byte in it wasn't UTF-8.
            let mut buf = Vec::new();
            let _ = std::io::BufReader::new(stderr).read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
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
    // Raw copy of every stdout line, kept independent of JSON parsing so a
    // crash mid-stream (or an unparseable line) still leaves something to
    // inspect — the parsed path above silently drops anything it can't read.
    let mut stdout_raw = String::new();
    {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(child.stdout.take().unwrap());
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                // A read error (e.g. invalid UTF-8 from `BufRead::lines`, a
                // broken pipe) used to break silently, leaving `stdout_raw`
                // truncated with nothing to say why — indistinguishable from
                // the process simply having nothing more to say. Recording
                // the reason inline means a crash-stdout.log that stops
                // mid-stream is diagnosable instead of a second mystery on
                // top of the one it was captured to explain.
                Err(e) => {
                    stdout_raw.push_str(&format!("[stdout read error: {e}]\n"));
                    break;
                }
            };
            stdout_raw.push_str(&line);
            stdout_raw.push('\n');
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

    // Prefer the `result` event's payload (the normal success path), but fall
    // back to the last `StructuredOutput` tool call the model emitted during
    // the stream when it's empty or unparseable. This matters most for the
    // budget-exhausted case: the claude CLI exits 1 and leaves
    // `result_event.result` empty even when the model DID produce a valid
    // `StructuredOutput` call before being cut off, and treating that as a
    // hard package failure throws away a usable fix for no good reason.
    let resp = match pick_turn_response(&result_event.result, &tool_calls) {
        Some(r) => r,
        None => {
            return Err(if !status.success() {
                Error::Failed {
                    code: status.code().unwrap_or(-1),
                    stderr: stderr_content,
                    stdout: stdout_raw,
                    prompt: prompt.clone(),
                }
            } else {
                Error::Parse {
                    message: format!(
                        "no usable turn response: result event is {:?} \
                         and no StructuredOutput tool call was made",
                        result_event.result
                    ),
                    stdout: stdout_raw,
                    prompt: prompt.clone(),
                }
            });
        }
    };
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
        "use_upstream_wheel" => Action::UseUpstreamWheel { why: resp.why },
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

/// Review-prompt branch for a non-Python package (plain C/C++ CMake/autotools
/// project, or a standalone Rust crate). The ordinary Review prompt is
/// built around pyproject.toml, `%{python_module ...}` macros, `%pytest`,
/// `%python_subpackages`, `%fdupes` sitelib/sitearch and so on — a wall of
/// text that reduces to "no_change" at the first checklist item for a
/// package that has none of those shapes (real case: drogon, run
/// `20261005-145029-f054b0`). This branch keeps only the items that still
/// apply (license, Source0 extension, sibling projects) and hands the rest
/// off to the appropriate `packaging-skills/` entry.
fn review_prompt_non_python() -> &'static str {
    "Review the generated spec before the first build.\n\
     \n\
     This is NOT a Python package — the RPM name has no `python-` prefix and \
     the generator found no Python packaging metadata under `source/`. Do not \
     read Python packaging files, do not touch python-module macros, pytest \
     macros, python_subpackages, or sitelib/sitearch %fdupes targets: none of \
     them apply.\n\
     \n\
     ## Which skill applies\n\
     \n\
     Load exactly one of these up front — the detailed packaging rules live \
     there, not in this prompt:\n\
     \n\
     - **`plain-c-cpp-packaging`** — CMakeLists.txt / configure.ac / \
       meson.build at the source root. Covers `find_package`/`PKG_CHECK_MODULES` \
       → BuildRequires lookup, shared-library SONAME subpackage split, \
       `%ldconfig_scriptlets`, `%files devel` layout for `-devel`/`.pc`/`.so` \
       split, and how to report missing `-devel` deps in `unresolvable_packages` \
       (strip `lib`-prefix and `-devel` suffix: `libtrantor-devel` → `trantor`).\n\
     - **`maturin-packaging`** — `Cargo.toml` at the root AND a `[build-system] \
       requires` naming maturin (but the latter would normally mean `is_python` \
       was true; load this one only if the Cargo-based build has no Python \
       surface at all, otherwise it's a Python package after all).\n\
     - **`rust-cargo-packaging`** — bare `Cargo.toml` at the root with no \
       Python wrapper. Uses `_service` + cargo-vendor + `%cargo_build`/`%cargo_install`.\n\
     \n\
     ## Review Checklist\n\
     \n\
     Work through these in order. If one does not apply, say so in one clause; \
     do not invent work.\n\
     \n\
     1. **License**: if the `License:` field is `FIXME` or not a valid SPDX \
        expression, fix it from `source/LICENSE`/`source/COPYING` (convention 5).\n\
     \n\
     2. **Source0 extension**: `ls sdist/` and compare the actual tarball's \
        extension against the spec's `Source0:` line — the generator sometimes \
        writes `.zip` when the real file is `.tar.gz` (or vice versa). This \
        fails `%prep` with 'No such file or directory' on the very first build. \
        Fix `Source0:` to match the real filename now, before anything else \
        depends on it.\n\
     \n\
     3. **BuildRequires for library deps**: list `find_package(...)` from \
        CMakeLists.txt or `PKG_CHECK_MODULES(...)` from configure.ac, and \
        verify each has a matching `-devel` or `pkgconfig(...)` BuildRequires \
        in the spec. For deps NOT yet in OBS, report them in `unresolvable_packages` \
        using the UPSTREAM project name — strip `lib`-prefix and `-devel`/`-dev` \
        suffix (`libtrantor-devel` → `trantor`), because the resolver searches \
        GitHub for the upstream project, not the distro's devel-package wrapping. \
        Full details in the `plain-c-cpp-packaging` skill.\n\
     \n\
     4. **Shared library split**: if the project builds a `lib<name>.so.<ver>`, \
        a proper openSUSE package splits into a runtime subpackage \
        (`lib<name><soversion>`) carrying only the versioned `.so.*` symlinks, \
        plus `%{name}-devel` carrying headers + `.pc` + the unversioned `.so`. \
        See the `plain-c-cpp-packaging` skill for the exact layout and the \
        `%ldconfig_scriptlets` lines the runtime subpackage needs.\n\
     \n\
     5. **Sibling projects**, if the archive holds more than one — decide the \
        subpackage layout now, before the first build, because it changes \
        `%build`, `%install` and every `%files` section (convention 30).\n\
     \n\
     6. **%check**: if there is no automated test target (no `enable_testing()` \
        in CMakeLists.txt, no `make check` in configure.ac), note that in the \
        spec and leave `%check` empty rather than inventing one.\n\
     \n\
     If a check does not apply, say so in one clause; do not invent work."
}

fn build_turn_prompt(req: &TurnRequest<'_>) -> String {
    let task_desc = match req.task {
        Task::Review if !req.is_python => review_prompt_non_python(),
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
                 the real filename now, before anything else depends on it. If fixing this \
                 means pointing `Source0:` at a DIFFERENT filename than the one currently in \
                 the checkout (e.g. it was a plain sdist tarball but should be the upstream \
                 wheel itself — convention 32), editing the spec line is not the whole fix: \
                 download the real file into the checkout and `osc add` it in the same turn. \
                 A spec correctly pointing at a filename that does not exist on disk yet still \
                 fails `%prep` with the exact same error, just one turn later — the checkout \
                 does not gain a file just because the spec now names it correctly (real \
                 example: `python-nvidia-nccl-cu13`, where the wrong tarball was deleted but \
                 the correct 197MB wheel was never fetched in the same turn, costing an entire \
                 extra attempt to notice and fix).\n\
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
             0e. **Build-backend name in `[build-system] requires` copied verbatim, wrong**: \
                 the generator writes a `BuildRequires: %{python_module <backend>}` line for \
                 whatever `[build-system] requires` says, straight from pyproject.toml's own \
                 spelling — and that spelling is exactly as likely to be the NORMALIZED name \
                 rather than the PyPI DISPLAY name as any runtime dependency (convention 2), \
                 except here nothing in %check ever imports it to surface the mismatch early — \
                 dependency resolution just fails outright with an empty build log. Verify \
                 EVERY build-backend BuildRequires against PyPI now, before the first build; \
                 `flit_core` alone caused an otherwise-avoidable extra turn in eight separate \
                 packages in the corpus so far — more than any other single cause of a wasted \
                 turn found in this review. Known repeat offenders — verify these are hyphenated even \
                 though pyproject.toml spells them with an underscore: `flit_core` → \
                 `flit-core`, `tomli_w` → `tomli-w`, `uv_build` → `uv-build`, \
                 `backports.zstd` → `backports-zstd` (a dot, not even an underscore). Don't \
                 stop at this list — `curl -s https://pypi.org/pypi/<name>/json | jq .info.name` \
                 every backend name you have not already verified this session.\n\
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
             3. **Main runtime dependencies** from TWO sources, same split as test \
                dependencies below:\n\
                a. DECLARED: diff `[project.dependencies]` (pyproject.toml) or \
                   `install_requires` (setup.cfg/setup.py) against the spec's Requires/ \
                   BuildRequires. Also check a top-level `requirements.txt` — some projects \
                   declare real runtime dependencies there instead of (or in addition to) \
                   pyproject.toml, and the generator does not always read it.\n\
                b. UNDECLARED: grep the package's actual source code (not test files — that's \
                   item 4) for `import`/`from ... import` naming a third-party package absent \
                   from every declared-dependency source above. Upstream's own dependency list \
                   is sometimes incomplete, and a declared list looking complete is not the \
                   same as every import the real code performs actually being covered — a gap \
                   here surfaces as %check failing to even `import` the package, one turn later \
                   than it needed to.\n\
                The generator sometimes copies test/optional dependencies faithfully but drops \
                the package's own main dependencies entirely. Add any missing ones as \
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
             For a %check failure, follow conventions 10 and 11 IN ORDER: diagnose and \
             add missing test dependencies first (convention 10 — a BuildRequires or \
             fixing an uninstalled test helper is almost always the right fix for an \
             `ImportError`/`ModuleNotFoundError`/`fixture not found`), and only then \
             consider convention 11's test-skip ladder for tests that genuinely \
             cannot run in this environment (no network, no GPU, no display beyond \
             what xvfb-run supplies, no shipped fixture). Skipping before you have \
             checked what the test actually needs throws away coverage for a problem \
             a BuildRequires would have fixed — the Oct 6 review flagged the earlier \
             blanket \"prefer skipping over adding deps\" phrasing as a contradiction \
             with convention 10, which it was.\n\
             \n\
             The hint below is a heuristic extract of the failure; read build.log \
             yourself whenever the extract looks like it caught the wrong thing.\n\
             \n\
             Before concluding a prior fix did not work, rule out a stale build.log: \
             compare this attempt's build.log against the previous one (md5sum, or the \
             VM boot timestamp/line count) — if they are byte-for-byte identical, OBS \
             has not actually rebuilt against your last commit yet, and this log says \
             nothing about whether that fix worked. Re-run the build (or verify the fix \
             statically, e.g. a local `rpmbuild`/`rpmspec --parse` check) instead of \
             reverting a plausible fix on the strength of a stale log — two real cases \
             (`triton`'s `PYTHONPATH` fix, `python-duckdb`'s `%dir` fix) were both nearly \
             undone this way before the staleness was caught."
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
    let source_desc = if req.is_python {
        "THE UNPACKED SOURCE — read pyproject.toml etc. under here"
    } else {
        "THE UNPACKED SOURCE — CMakeLists.txt / configure.ac / Cargo.toml etc. live here"
    };
    let mut map_lines = Vec::new();
    for (name, desc, always) in &[
        (
            "spec",
            "the spec file — see \"Editing the spec\" below",
            true,
        ),
        (
            "spec.generated",
            "pristine generated spec for reference",
            false,
        ),
        (
            "build.log",
            "full osc build log from the last attempt",
            false,
        ),
        (
            "rpmspec.txt",
            "rpmspec --parse stderr from the last failure",
            false,
        ),
        ("source/", source_desc, false),
        (
            "sdist/",
            "the extraction root; `source/` is the directory inside it",
            false,
        ),
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
        format!(
            "## Prior attempt journal (notes.md)\n\n{}\n\n\
             Before proposing a fix, check whether it matches — or reverses — something \
             already listed above. Two or more entries here quietly flipping back and \
             forth on the same line (e.g. an env var vs. a `-D` CMake arg for the same \
             variable, or a package name tried both with `-` and `.`) is not \
             progress — it means neither attempt was verified against the actual build \
             log before the next one reverted it. If your fix would repeat or reverse a \
             prior entry, you MUST cite concrete evidence from THIS run's build.log \
             explaining what specifically was wrong with the earlier attempt — not just \
             restate the same reasoning with the opposite polarity. If you cannot point to \
             that evidence, the earlier attempt was likely never the problem; look \
             elsewhere in the log instead of alternating between the same two guesses.\n\n",
            req.notes
        )
    };

    let bullets = |deps: &[String]| {
        deps.iter()
            .map(|d| format!("  - {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let unresolvable_detail_section = if req.unresolvable_detail.is_empty() {
        String::new()
    } else {
        format!(
            "Raw OBS detail for the above (verbatim `nothing provides ...` / \
             `(got version ...)` lines):\n{}\n\n\
             Read this before assuming any name above is simply missing: if a line \
             names a version constraint (`< X`) and the next line says OBS already \
             `got version` something *newer* than `X`, this package is NOT missing — \
             the repository already has it, just not at a version this constraint \
             accepts. See item 6 below.\n\n",
            bullets(req.unresolvable_detail),
        )
    };

    let unresolvable_section = if req.unresolvable_deps.is_empty() {
        String::new()
    } else {
        format!(
            "## Missing BuildRequires\n\n\
             OBS could not find these RPM packages in any of its repositories:\n{}\n\n\
             {unresolvable_detail_section}\
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
             4. **A missing `python<NNN>-<dep>` is USUALLY a flavor gap, not a new package**: \
             `%{{python_module <dep>}}` expands to one BuildRequires per active Python flavor \
             on this build target, and a newer flavor sometimes lacks a subpackage that older \
             flavors already have (e.g. `python314-cython` missing while `python312-cython` \
             builds fine). Do NOT put the flavor-prefixed name in `unresolvable_packages` — the \
             pipeline strips the `python<NNN>-` prefix before treating a name as buildable, so \
             reporting `python314-cython` just queues a redundant duplicate build of `cython`, \
             which already exists. Leave the BuildRequires as `%{{python_module <dep>}}` in the \
             spec; the gap closes on its own once that flavor catches up. **But check the raw \
             detail above first** — if OBS's `(got version ...)` line shows a version that \
             already satisfies (or exceeds) what a real flavor gap would need, this is item 6's \
             case instead, and leaving the spec alone will never close it.\n\
             5. **Fix the spec**: If the spec incorrectly uses `%{{python_module <system-tool>}}`, \
             remove the macro and use the plain package name instead. Do NOT report these \
             as unresolvable.\n\
             6. **A version constraint YOUR OWN spec wrote can make a real, present package look \
             missing**: `nothing provides python314-setuptools < 70.0.0 (got version 80.9.0-3.3)` \
             means OBS DOES have `setuptools` — installed 80.9.0 — but this spec's own \
             `BuildRequires: %{{python_module setuptools < 70.0.0}}` (usually copied verbatim from \
             `[build-system] requires` in pyproject.toml) demands something older. A rolling-release \
             distro ships exactly one current version of a package; it can never satisfy a `<` \
             upper bound once that version has moved past it, no matter how many times you report \
             it. Do NOT put this name in `unresolvable_packages` — reporting it changes nothing, \
             since the pipeline sees the package already exists. Fix the spec instead: drop the \
             upper bound entirely (`%{{python_module setuptools}}`) and let `%check`/the actual \
             build prove whether the newer version really breaks anything; only reach for a \
             version-specific workaround if it demonstrably does.\n\n\
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

    // Ground truth the pipeline itself already verified — not a guess the
    // agent could be wrong about — so the guidance is unconditional, unlike
    // `failed_section`'s "skip the tests/feature" (these are required
    // BuildRequires, not optional test deps; there is no feature to drop).
    let unbuildable_section = if req.unbuildable_deps.is_empty() {
        String::new()
    } else {
        format!(
            "## Dependencies that can never be built here\n\n\
             These have no PyPI project and no discoverable public source \
             repository at all — confirmed, not a guess — so this pipeline can \
             never build them no matter how many times they're reported: \
             {}\n\n\
             This is almost always a proprietary vendor SDK/compiler (NVIDIA's \
             `cuda-devel`/`cuda-runtime` are the running example — the actual \
             CUDA compiler toolkit, which only exists as a system package this \
             pipeline cannot create). Do NOT put any of these in \
             `unresolvable_packages` — that restarts a cycle that can only ever \
             end the same way. If your spec's `%build` genuinely needs one of \
             these to compile from source, that means THIS package cannot be \
             compiled in this environment at all: check whether PyPI publishes \
             a prebuilt wheel for this package at this version and, if so, set \
             action to `\"use_upstream_wheel\"` per convention 28 Exception 4 \
             instead of continuing to report the dependency.\n\n",
            bullets(req.unbuildable_deps),
        )
    };

    format!(
        "You are fixing an openSUSE RPM spec for the package {pkg} {version}.\n\
         Task: {task}\n\
         \n\
         {budget}\n\
         \n\
         **GOAL**: Get this package built. Report ALL missing dependencies in \
         `unresolvable_packages` — each gets packaged in turn, which may reveal more, \
         recursively, until the whole dependency tree is satisfied.\n\
         \n\
         {source_note}\
         ## Workspace\n\
         {map}\n\
         \n\
         {spec_section}\
         {subprojects}\
         {notes}\
         {unresolvable}\
         {failed}\
         {unbuildable}\
         {context}\
         {hint}",
        pkg = req.pkg_name,
        version = req.version,
        task = task_desc,
        budget = budget_line,
        map = workspace_map,
        source_note = source_note,
        subprojects = subproject_section,
        spec_section = spec_section,
        notes = notes_section,
        unresolvable = unresolvable_section,
        failed = failed_section,
        unbuildable = unbuildable_section,
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
            is_python: true,
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
            unresolvable_detail: &[],
            failed_deps: &[],
            unbuildable_deps: &[],
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

        assert!(
            prompt.contains("## Other projects in this archive"),
            "{prompt}"
        );
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

    /// Regression test for the whole point of `static_system_prompt`: the
    /// per-turn user prompt must NOT contain the static conventions/workflow
    /// text, or re-inlining it here would silently defeat prompt caching
    /// (Anthropic's cache only hits on a byte-identical prefix, and this
    /// prompt differs on every call by construction — package name, spec
    /// content, notes.md, ...). The static content belongs only in
    /// `--append-system-prompt`, sent once per turn as its own argument.
    #[test]
    fn static_conventions_and_workflow_text_are_not_inlined_into_the_user_prompt() {
        let ws = std::path::Path::new("/nonexistent-workspace");
        let prompt = build_turn_prompt(&request(ws, &[]));
        assert!(
            !prompt.contains("29. Missing source code"),
            "a CONVENTIONS entry leaked into the per-turn user prompt"
        );
        assert!(
            !prompt.contains("### Finding Missing C Headers and Libraries"),
            "STATIC_WORKFLOW leaked into the per-turn user prompt"
        );
    }

    #[test]
    fn static_system_prompt_carries_both_conventions_and_workflow() {
        let combined = static_system_prompt();
        assert!(combined.contains("29. Missing source code"));
        assert!(combined.contains("### Finding Missing C Headers and Libraries"));
        assert!(combined.contains("## Workflow"));
    }

    /// Real shape from a `drogon` build where the claude CLI hit its budget
    /// cap AFTER the model had already emitted a valid `StructuredOutput`
    /// call: the final `result` event's `result` field was empty and the CLI
    /// exited 1, so the previous `turn()` returned `Error::Failed` and the
    /// pipeline logged "agent error: claude exited 1" even though the model's
    /// answer was sitting right there in the tool-call stream.
    #[test]
    fn pick_turn_response_falls_back_to_structured_output_tool_call() {
        let tool_calls = vec![
            ToolCall {
                name: "Read".into(),
                input: "{}".into(),
            },
            ToolCall {
                name: "StructuredOutput".into(),
                input: r#"{
                    "action": "no_change",
                    "reasoning": "deps missing — report trantor",
                    "evidence": ["Read drogon.spec"],
                    "confidence": "high",
                    "unresolvable_packages": ["trantor"]
                }"#
                .into(),
            },
        ];
        let resp = pick_turn_response("", &tool_calls)
            .expect("fallback must salvage the StructuredOutput call");
        assert_eq!(resp.action, "no_change");
        assert_eq!(resp.unresolvable_packages, vec!["trantor"]);
    }

    #[test]
    fn pick_turn_response_prefers_result_payload_over_tool_call() {
        let tool_calls = vec![ToolCall {
            name: "StructuredOutput".into(),
            input: r#"{"action":"no_change","reasoning":"stale","evidence":[],"confidence":"low"}"#
                .into(),
        }];
        let resp = pick_turn_response(
            r#"{"action":"edited","reasoning":"fresh","evidence":["x"],"confidence":"high"}"#,
            &tool_calls,
        )
        .expect("result payload must take priority");
        assert_eq!(resp.action, "edited");
        assert_eq!(resp.reasoning, "fresh");
    }

    #[test]
    fn pick_turn_response_returns_none_when_nothing_usable() {
        assert!(pick_turn_response("", &[]).is_none());
        assert!(
            pick_turn_response(
                "",
                &[ToolCall {
                    name: "Read".into(),
                    input: "{}".into()
                }]
            )
            .is_none()
        );
    }

    /// Real-shape regression from drogon run `20261005-145029-f054b0`: Haiku
    /// described the structured response in prose instead of calling the tool,
    /// so both JSON paths found nothing and the four reported C++ libraries
    /// (trantor, jsoncpp, libbrotli, yaml-cpp) got discarded. The prose parser
    /// recovers at least the actionable `unresolvable_packages` list.
    #[test]
    fn pick_turn_response_parses_prose_when_tool_call_was_skipped() {
        let text = "I've already called the StructuredOutput tool at the end of my response. \
                    The structured output includes:\n\
                    \n\
                    - **action**: `no_change`\n\
                    - **reasoning**: The spec is correct; deps missing in OBS\n\
                    - **evidence**: Read drogon.spec, Read CMakeLists.txt\n\
                    - **confidence**: `high`\n\
                    - **unresolvable_packages**: `trantor`, `jsoncpp`, `libbrotli`, `yaml-cpp`\n";
        let resp = pick_turn_response(text, &[])
            .expect("prose fallback must salvage the four reported deps");
        assert_eq!(resp.action, "no_change");
        assert_eq!(resp.confidence, Confidence::High);
        assert_eq!(
            resp.unresolvable_packages,
            vec!["trantor", "jsoncpp", "libbrotli", "yaml-cpp"]
        );
        assert_eq!(
            resp.evidence,
            vec!["Read drogon.spec", "Read CMakeLists.txt"]
        );
    }

    /// A non-Python package (drogon-shape: no `python-` RPM prefix) must get
    /// the shorter non-Python Review prompt. The Python checklist mentions
    /// pyproject.toml, `%python_module`, `%pytest` — none of which apply.
    #[test]
    fn review_prompt_drops_python_checklist_for_non_python_packages() {
        let tmp = std::env::temp_dir().join("ap-build-review-nonpython-test");
        let _ = std::fs::create_dir_all(&tmp);
        // `source/` exists for a real review turn — make sure the workspace
        // map description doesn't tell the agent to read pyproject.toml under
        // a non-Python package's `source/`.
        let _ = std::fs::create_dir_all(tmp.join("source"));
        let subprojects: Vec<(String, String)> = Vec::new();
        let mut req = request(&tmp, &subprojects);
        req.is_python = false;
        req.pkg_name = "drogon";
        let prompt = build_turn_prompt(&req);
        assert!(
            prompt.contains("This is NOT a Python package"),
            "non-Python Review prompt should announce the branch; got:\n{prompt}"
        );
        assert!(
            prompt.contains("plain-c-cpp-packaging"),
            "non-Python Review prompt should point at the plain-c-cpp skill"
        );
        // The Python checklist's defining items must not appear:
        for python_term in [
            "pyproject.toml",
            "%{python_module",
            "%python_subpackages",
            "%pytest",
            "python_sitelib",
            "console_scripts",
        ] {
            assert!(
                !prompt.contains(python_term),
                "non-Python Review prompt still contains Python-only term {python_term:?}"
            );
        }
    }

    /// A Python package must still get the full Python checklist.
    #[test]
    fn review_prompt_keeps_python_checklist_for_python_packages() {
        let tmp = std::env::temp_dir().join("ap-build-review-python-test");
        let _ = std::fs::create_dir_all(&tmp);
        let subprojects: Vec<(String, String)> = Vec::new();
        let req = request(&tmp, &subprojects); // is_python: true from request()
        let prompt = build_turn_prompt(&req);
        assert!(prompt.contains("pyproject.toml"));
        assert!(prompt.contains("%{python_module"));
    }

    #[test]
    fn prose_fallback_requires_an_action_marker() {
        // A plain acknowledgement with no **action**: line is just chatter —
        // treating it as `no_change` would mask real bugs upstream.
        let text = "The review is complete. See you later.";
        assert!(pick_turn_response(text, &[]).is_none());
    }

    // ── scrub_env_for_agent ───────────────────────────────────────────────────

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn scrub_env_keeps_anthropic_api_key() {
        let scrubbed = scrub_env_for_agent(vec![pair("ANTHROPIC_API_KEY", "sk-ant-x")], &[]);
        assert_eq!(
            scrubbed.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant-x")
        );
    }

    #[test]
    fn scrub_env_drops_osc_password() {
        // OSC_PASSWORD is a publishing credential — the agent must never see
        // it. OSC pushes happen in the pipeline's own subprocesses, not the
        // agent's turn.
        let scrubbed = scrub_env_for_agent(vec![pair("OSC_PASSWORD", "hunter2")], &[]);
        assert!(
            !scrubbed.contains_key("OSC_PASSWORD"),
            "OSC_PASSWORD must not pass through"
        );
    }

    #[test]
    fn scrub_env_drops_aws_secret_access_key() {
        // Cloud credentials — not in the allow-list and matches *_SECRET too.
        let scrubbed = scrub_env_for_agent(vec![pair("AWS_SECRET_ACCESS_KEY", "AKIA...")], &[]);
        assert!(
            !scrubbed.contains_key("AWS_SECRET_ACCESS_KEY"),
            "AWS_SECRET_ACCESS_KEY must not pass through"
        );
    }

    #[test]
    fn scrub_env_drops_arbitrary_token_pattern() {
        // The suffix matcher catches unknown credential-shaped names that no
        // one thought to list explicitly — e.g. some new forge's auth token.
        let scrubbed = scrub_env_for_agent(vec![pair("FOO_TOKEN", "ghp_abc")], &[]);
        assert!(
            !scrubbed.contains_key("FOO_TOKEN"),
            "FOO_TOKEN must be dropped by the suffix guard"
        );
    }

    #[test]
    fn scrub_env_keeps_path() {
        let scrubbed = scrub_env_for_agent(vec![pair("PATH", "/usr/bin:/bin")], &[]);
        assert_eq!(
            scrubbed.get("PATH").map(String::as_str),
            Some("/usr/bin:/bin")
        );
    }

    #[test]
    fn scrub_env_keeps_home() {
        let scrubbed = scrub_env_for_agent(vec![pair("HOME", "/home/u")], &[]);
        assert_eq!(scrubbed.get("HOME").map(String::as_str), Some("/home/u"));
    }

    #[test]
    fn scrub_env_drops_unknown_benign_var() {
        // Strict allow-list: SOMETHING_IRRELEVANT doesn't look like a secret
        // and doesn't appear in the allow-list, so it is dropped. The agent
        // should run in a maximally minimal environment.
        let scrubbed = scrub_env_for_agent(vec![pair("SOMETHING_IRRELEVANT", "value")], &[]);
        assert!(
            !scrubbed.contains_key("SOMETHING_IRRELEVANT"),
            "unknown non-allowlisted names are dropped under the strict allow-list"
        );
    }

    #[test]
    fn scrub_env_passes_through_every_allowlist_entry() {
        // Every entry in the explicit allow-list, each with a distinct value,
        // must survive scrubbing. Regression guard: if someone adds a name
        // that collides with the credential-suffix patterns (e.g. a
        // hypothetical `AGENT_API_KEY`), this test will spot it.
        let input: Vec<(String, String)> = AGENT_ENV_ALLOWLIST
            .iter()
            .enumerate()
            .map(|(i, name)| ((*name).to_string(), format!("v{i}")))
            .collect();
        let scrubbed = scrub_env_for_agent(input.clone(), &[]);
        for (name, expected) in &input {
            assert_eq!(
                scrubbed.get(name).map(String::as_str),
                Some(expected.as_str()),
                "allow-list entry {name} failed to survive scrubbing"
            );
        }
    }

    #[test]
    fn scrub_env_credential_suffix_overrides_extra_allow() {
        // The extension point for a future `--pass-env` flag still runs
        // through the credential-suffix guard: a secret-shaped name does NOT
        // get forwarded just because someone put it on the CLI.
        let extra = vec!["MYSERVICE_TOKEN".to_string()];
        let scrubbed = scrub_env_for_agent(vec![pair("MYSERVICE_TOKEN", "secret")], &extra);
        assert!(
            !scrubbed.contains_key("MYSERVICE_TOKEN"),
            "credential-suffix guard must override extra_allow"
        );
    }

    #[test]
    fn scrub_env_extra_allow_forwards_benign_names() {
        // A benign extra-allow name DOES pass through — proof the extension
        // point is wired up for real, not just dead code.
        let extra = vec!["MYSERVICE_HOST".to_string()];
        let scrubbed = scrub_env_for_agent(vec![pair("MYSERVICE_HOST", "example.com")], &extra);
        assert_eq!(
            scrubbed.get("MYSERVICE_HOST").map(String::as_str),
            Some("example.com")
        );
    }

    /// Every `convention N` reference must name a convention that exists.
    #[test]
    fn convention_cross_references_resolve() {
        let count = CONVENTIONS
            .lines()
            .filter_map(|l| {
                l.split_once(". ")
                    .and_then(|(h, _)| h.parse::<usize>().ok())
            })
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
