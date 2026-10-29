//! The user prompt's pieces: the review prompt for a package that is not Python,
//! and the fence for untrusted data.

// ── prompt construction ───────────────────────────────────────────────────────

/// Review-prompt branch for a non-Python package (plain C/C++ CMake/autotools
/// project, or a standalone Rust crate). The ordinary Review prompt is
/// built around pyproject.toml, `%{python_module ...}` macros, `%pytest`,
/// `%python_subpackages`, `%fdupes` sitelib/sitearch and so on — a wall of
/// text that reduces to "no_change" at the first checklist item for a
/// package that has none of those shapes. This branch keeps only the items that still
/// apply (license, Source0 extension, sibling projects) and hands the rest
/// off to the appropriate `packaging-skills/` entry.
pub(super) fn review_prompt_non_python() -> &'static str {
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
     4. **`-devel` split (mandatory whenever headers are installed)**: if the \
        project installs ANY header (`install(DIRECTORY include ...)`, \
        `install(FILES *.h/*.hpp)`, `make install` into `/usr/include`), a \
        `%package devel` is required — headers, `.pc` and `lib/cmake/` files go \
        there, never in the main package. This holds with or without a SOVERSION, \
        for static or shared libs, and for header-only libs. Without it rpmlint \
        `devel-file-in-non-devel-package` (Badness 50 per header) fails the build. \
        Do NOT drop or merge `-devel` to work around a build error, and do NOT \
        reason \"no SOVERSION, so no split\" — that only removes the runtime \
        `lib<name><soversion>` subpackage, never `-devel`. If the project builds a \
        `lib<name>.so.<ver>`, additionally add the runtime subpackage \
        (`lib<name><soversion>`) carrying only the versioned `.so.*`, with \
        `%{name}-devel` carrying headers + `.pc` + the unversioned `.so`. \
        See the `plain-c-cpp-packaging` skill for the exact layout and the \
        `%ldconfig_scriptlets` lines the runtime subpackage needs. Every \
        package-private install directory (e.g. `%{_libdir}/<name>-X.Y/`) must \
        be owned exactly once — `%dir` it, or list the whole directory; listing \
        only files inside it fails `check-filelist` ('directories not owned by a \
        package'), and listing it twice warns 'File listed twice'.\n\
     \n\
     5. **Sibling projects**, if the archive holds more than one — decide the \
        subpackage layout now, before the first build, because it changes \
        `%build`, `%install` and every `%files` section (convention 30).\n\
     \n\
     6. **%check**: if there is no automated test target (no `enable_testing()` \
        in CMakeLists.txt, no `make check` in configure.ac), note that in the \
        spec: omit `%check` and put a `# no upstream test suite` comment where \
        it would go (matches the plain-c-cpp-packaging skill) rather than inventing \
        one. Never write TODO in the spec.\n\
     \n\
     If a check does not apply, say so in one clause; do not invent work."
}

/// Wrap untrusted text (build log excerpts, error lines) in a code fence the
/// text cannot close. The fence is one backtick longer than the longest run of
/// backticks in `text`, so no line of the content can end the block early.
pub(crate) fn fenced_data(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}\n")
}
