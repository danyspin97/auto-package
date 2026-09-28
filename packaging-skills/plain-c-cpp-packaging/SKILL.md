---
name: plain-c-cpp-packaging
description: Use when packaging a plain C/C++ library or binary with no Python and no Cargo involved at all — just a CMakeLists.txt, configure.ac, or meson.build at the source root (e.g. a C library dependency that itself needs to be built from source, unresolvable_packages-style). Covers shared-library SONAME subpackage splits, %ldconfig_scriptlets, CMake/autotools/meson %build patterns, source signature verification, and %files layout for devel/runtime splits.
---

# Plain C/C++ packages

A third ecosystem, much less uniform than Python or Rust. The generator only
picks the build system and the `%autosetup -n` directory; everything else is
a TODO to fill in from `source/`. The shapes below are taken from real
openSUSE specs (`fmt`, `zlib-ng`, `libuv` in Factory) — verify the exact macro
spelling with `rpmspec --parse` rather than assuming these examples
generalize perfectly to this package.

- **Shared library or not?** Grep `CMakeLists.txt` for
  `add_library(... SHARED ...)` (CMake defaults to `STATIC` unless
  `BUILD_SHARED_LIBS` is set) or `configure.ac` for `LT_INIT`/
  `AC_PROG_LIBTOOL` (libtool-managed shared libraries). If it only builds
  a binary or a static library, one package with no split is enough —
  skip the rest of this list.
- **Shared library split**: a versioned shared library needs its own
  `%package -n lib<name><soversion>` (the soname's major version, e.g.
  `libfmt12`, `libz1-ng`, `libuv1` — find it from `set(SOVERSION ...)`/
  `VERSION`+`SOVERSION` in CMakeLists.txt, or `-version-info` in
  configure.ac/Makefile.am) plus a `%package devel` with
  `Requires: lib<name><soversion> = %{version}`. The base `Name:` package
  can stay empty (metapackage) or hold a CLI binary if there is one.
- **`%ldconfig_scriptlets`**: every shared-library subpackage needs
  `%ldconfig_scriptlets -n lib<name><soversion>` (expands to the
  `%post`/`%postun -p /sbin/ldconfig` pair) directly before its `%files`
  section. Do not hand-write the ldconfig calls — the macro is the current
  idiom (do not invent macros; this one is real, confirmed on the build
  host's `/usr/lib/rpm/macros.d/macros.ldconfig`).
- **CMake `%build`/`%install`**: `%cmake` (configure) then `%cmake_build`,
  then `%cmake_install`. Never hand-write `mkdir build && cd build && cmake
  ..` — the macros already do the out-of-tree build and set the standard
  flags (`CMAKE_INSTALL_PREFIX`, `CMAKE_BUILD_TYPE`, etc.).
- **Meson `%build`/`%install`/`%check`**: the same idea, different macro set —
  `%meson` (configure) then `%meson_build`, `%meson_install`, and
  `%meson_test` for `%check`. Detect via `meson.build` at the source root.
  Never hand-write `meson setup`/`ninja` invocations.
- **autotools `%build`/`%install`**: if the archive is a repository
  snapshot (no generated `configure` script — the normal case for a GitHub
  tag archive, as opposed to a `make dist` release tarball), run
  `autoreconf -fiv` first, then `%configure`, then `%make_build`. Install
  with `%make_install`; delete `*.la` files from the buildroot afterwards
  (libtool archives are never packaged).
- **`%check`**: `%ctest` for CMake projects that call `enable_testing()`/
  `add_test()`; `%make_build check` (sometimes gated behind
  `%if !0%{?qemu_user_space_build}` when the suite needs the build arch's
  native execution) for autotools projects with a `check` target. If
  neither exists, say so and leave `%check` out with a `# TODO:` comment
  — never `|| true`. For autotools specifically, dump the real test log on
  failure instead of leaving only a generic "check failed" in build.log —
  autotools' own `tests/testsuite.log`/`test-suite.log` has the actual
  per-test output and is otherwise easy to miss:
    %make_build check || { cat tests/testsuite.log; exit 1; }
  (real example: `tar`).
- **BuildRequires for library dependencies**: `find_package(Foo)` in
  CMakeLists.txt or `PKG_CHECK_MODULES(FOO, foo)` in configure.ac each need
  the matching `-devel` package or `pkgconfig(foo)` virtual provide — the
  same lookup approach as for a Python C-extension (`zypper search
  --provides`/`zypper what-provides`). Do not guess the -devel package
  name; verify it.
- **`%files`**: `%license` for the actual license file in `source/`
  (`LICENSE`, `COPYING`, `COPYING.LESSER`), `%doc` for `README`/`AUTHORS`/
  `ChangeLog` if present, `%{_includedir}/...` and
  `%{_libdir}/pkgconfig/*.pc` in `%files devel`, and `%{_libdir}/lib<name>.so.*`
  (the versioned SONAME symlink, not the unversioned `.so` — that one goes
  in `%files devel` since it's only needed at link time) in the shared
  library subpackage.
- **Source signature verification**: for a security-relevant system library
  fetched directly from its own release page (rather than a registry like
  PyPI with its own trust model), fetching the upstream GPG signature and
  maintainer keyring alongside the tarball is standard openSUSE practice,
  not optional hardening — OBS verifies the signature against the keyring
  automatically when both are present:
    Source:  <tarball>
    Source1: <tarball>.sig
    Source2: <name>.keyring
  (real examples: `acl`, `attr`, `coreutils`, `bash`, `elfutils`). Worth
  adding when packaging a widely-depended-on library from source, not
  required for every single one-off dependency.

## Things you may see in reference specs that rarely matter for a one-off dependency build

auto-package is not limited to a single architecture — `--arch` is a plain
CLI argument, and a target project can build for x86_64, aarch64, or others.
These items are real, current openSUSE idioms — reproduce them if you're
mirroring an existing Factory/Base:System spec closely — but they affect
downstream consumers' compatibility (on some OTHER architecture or
configuration than the one this build is currently targeting), not whether
THIS package builds successfully on its own target arch right now. So don't
spend an attempt chasing them for an ad-hoc missing-dependency build unless
the task specifically calls for it:

- **`baselibs.conf`**: a shared library aimed at being widely reused as a
  build dependency ships a sibling `Source: baselibs.conf` file (its own
  small DSL, not spec logic) so OBS auto-generates 32-bit-compat
  subpackages alongside the 64-bit build. Worth adding if this library is
  likely to be a dependency for other x86 packages needing 32-bit
  compatibility; does not affect whether the primary build succeeds.
- **PAM modules**: install to `%{_pam_vendordir}` (the vendor default,
  always shipped) in addition to `%{_sysconfdir}/pam.d/<name>` marked
  `%config(noreplace)` (the mutable admin copy) on `0%{?suse_version} > 1500`
  targets — see `sudo`/`at` for the full dual-placement dance. Only
  relevant if the package itself ships a PAM module.
- **udev rules**: install to `%{_udevrulesdir}` with
  `BuildRequires: pkgconfig(udev)` — only relevant for a library/daemon
  that talks to hardware directly.
