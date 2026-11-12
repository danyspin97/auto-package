//! The task text of the two review turns: the pre-build review of a Python
//! package and the post-build clean-up. Kept apart from build_prompt.rs.

use super::*;

/// The pre-build review of a Python package, before the first build.
pub(super) fn review_prompt_python() -> &'static str {
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
                 convention 6 (section 4)) for the full treatment — Source1 registry.tar.zst, \
                 cargo-packaging, ExclusiveArch, CARGO_* env vars, etc. — and apply ALL of it \
                 before checking anything else; these changes affect the entire spec structure.\n\
             \n\
             0c. **Source0 extension mismatch**: `ls sdist/` and compare the actual tarball's \
                 extension against the spec's `Source0:` line — the generator sometimes writes \
                 `.zip` when the real file is `.tar.gz` (or vice versa). This fails `%prep` with \
                 'No such file or directory' on the very first build. Fix `Source0:` to match \
                 the real filename now, before anything else depends on it. You cannot \
                 download files or `osc add` them (neither is in your tool list): if \
                 `Source0:` must name a file that is not in the checkout at all, do not \
                 just rename the line — a spec pointing at a missing file fails `%prep` \
                 with the same error one turn later (real example: \
                 `python-nvidia-nccl-cu13`). Say exactly which file is missing and return \
                 use `stage_files` when it is in `source/`, `sdist/` or a dependency's \
                 tree, otherwise a `download_files` Source or `_service` entry (or \
                 `use_upstream_wheel` when convention 28 Exception 4 applies); the \
                 pipeline fetches sources outside your turn.\n\
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
                 stop at this list — WebFetch `https://pypi.org/pypi/<name>/json` (`info.name`) \
                 every backend name you have not already verified this session.\n\
             \n\
             0f. **CUDA needed to BUILD**: `Grep` the source for `find_package(CUDAToolkit`, \
                 `enable_language(CUDA)`, `nvcc`, `CUDAExtension`, compiled `.cu` files, and \
                 `#include <cuda` / `<cub/` / `<nccl.h>` in compiled sources, and check the \
                 spec's BuildRequires for `cuda-devel`/`cuda-runtime`. If the BUILD needs \
                 CUDA, stop reviewing a source build: this package is repackaged from its \
                 upstream wheel — return `use_upstream_wheel` now (convention 32 (section 84) CUDA special \
                 case; convention 32 (section 49) when the wheels are per-CPython). Run-time-only use \
                 (NVRTC/Triton JIT, `dlopen`) is not a build need.\n\
             \n\
             ## Main Review Checklist\n\
             \n\
             The generator switched to arch macros if it saw C/C++/Cython source — that's \
             a guess, so confirm it against convention 3 first. Then work through these in order:\n\
             \n\
             1. **The import name** the package installs — convention 17 (section 16). This is the \
                single most common thing the generator gets wrong. Always verify from \
                `source/*.egg-info/top_level.txt` (definitive) or the source code. Never \
                derive it from the PyPI package name.\n\
             \n\
             2. **Console scripts**: Read `[project.scripts]` or `entry_points` in \
                source/pyproject.toml or source/setup.cfg. If ANY scripts are declared, \
                verify ALL python_alternative components are present (convention 107 (section 18)):\n\
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
                AND `Requires: python-<name>` (needed at install time; flavor-neutral, see convention 8 (section 65)). Do NOT copy \
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
                convention 7 (section 12). Fix these now rather than after a failed build.\n\
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
                %build, %install and every %files section (convention 22 (section 30)).\n\
             \n\
             11. **%description** — the generator writes the Summary into it, and the pipeline \
                sends the spec back for a whole extra turn when it still equals the Summary \
                (most packages so far). Write a real one now, in one to three plain \
                sentences taken from the README's first paragraph (convention 63 (section 66)).\n\
             \n\
             If a check does not apply, say so in one clause; do not invent work."
}

/// The clean-up pass over a spec that has just built.
pub(super) fn post_build_review_prompt() -> &'static str {
    "The package just BUILT. This is an optional clean-up pass over the spec, not a \
             fix: the build is green and must stay green. Any edit costs a full rebuild, and \
             a rebuild that fails throws your edit away, so edit only for a clear, safe win.\n\
             \n\
             The Context below holds the diff of the spec against the generated one (what \
             the earlier turns changed) and the warnings of the build. Look for: debug \
             leftovers added to diagnose a failure (`config.log` dumps, `ls`/`cat`/`set -x` \
             lines, `|| true` that hides errors); a workaround of a failed attempt that the \
             journal shows was a wrong guess and that the build does not need; a deselected \
             or skipped test without a comment naming the reason; a BuildRequires or Requires \
             that nothing uses; `%files` entries listed twice (on a new spec, also one that \
             is too broad); real rpmlint warnings that a spec change fixes.\n\
             \n\
             Do NOT remove anything that fixed a build failure unless the log proves it \
             unnecessary, do not reformat, reorder or rewrite working lines, do not touch \
             Version, sources or patches, and do not read the whole build log. If nothing \
             clearly improves the package, return `no_change` without investigating further."
}

/// The pre-build review task: an update of a carried-forward spec gets its
/// short checklist, a new spec the full one for its language.
pub(super) fn review_task(req: &TurnRequest<'_>) -> &'static str {
    if req.carried {
        review_prompt_update()
    } else if req.is_python {
        review_prompt_python()
    } else {
        review_prompt_non_python()
    }
}

/// The pre-build review of a carried-forward spec: the maintainer's recipe
/// with only `Version:` and the source changed. Lists what may be edited on
/// this update and what must stay, and nothing else.
pub(super) fn review_prompt_update() -> &'static str {
    "Review the carried-forward spec before the first build. This is an UPDATE: the \
     spec is the package's existing recipe, and the Context says what this run has \
     changed in it. Its maintainer chose its form. The job is to let the new release \
     build with the fewest edits, not to bring the spec to the shape of a new one.\n\
     \n\
     ## Allowed edits\n\
     \n\
     Only these, on this update:\n\
     - `Version:`, the `Source` lines and `_service` for the new release. A `Source0:` \
       extension that does not match the file in `sdist/` is fixed (`ls sdist/`).\n\
     - Patches: one added, refreshed or dropped, each named in the changelog field.\n\
     - BuildRequires and Requires that the new upstream build files demand: read \
       `source/pyproject.toml`, `setup.cfg`, `setup.py`, `CMakeLists.txt`, \
       `configure.ac`, `meson.build` or `Cargo.toml` and add or drop only what they \
       now declare, spelled as the spec spells its neighbours.\n\
     - `%files` entries for files that appeared or vanished in the new release, and \
       a soname rename when upstream changes the soversion (convention 86).\n\
     - The `-n` directory of `%setup` or `%autosetup`, when the new release unpacks \
       to another directory name.\n\
     - `License:`, when upstream changed the licence: the SPDX expression the new \
       source states (convention 5).\n\
     - Build or test flags the new version needs, and a `%check` skip that the new \
       tests no longer need to keep.\n\
     \n\
     ## Do not touch\n\
     \n\
     Even where a convention or a packaging skill would write it differently:\n\
     - `Group:`, `BuildRoot:`, `%clean`, `%defattr`, unless a build or rpmlint failure \
       of this version forces the change (convention 90).\n\
     - The form of a scriptlet (`%ldconfig_scriptlets` or a hand-written `%post`), \
       except the name a soname rename forces (`%ldconfig_scriptlets -n lib<name><soversion>`, \
       `%post -n lib...`).\n\
     - The glob style of `%files`: do not narrow or widen a glob.\n\
     - `Summary:`, `%description`, the copyright and licence header, and comments: \
       do not reword one. Do not add a comment, except the reason a new skip or a \
       dropped bound needs (conventions 11 and 75), and never one that restates a \
       comment the spec already has.\n\
     - The spelling of a Requires that is already there (`python3dist(...)`, \
       `%{python_module ...}`).\n\
     - A `%license` or licence-file line that no build gate asks for.\n\
     \n\
     Anything that a failure of this version forces is fixed after the build, not \
     here. If nothing in the allowed list applies, say so in one clause and return \
     `no_change`."
}
