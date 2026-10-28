---
name: plain-c-cpp-packaging
description: Use when packaging a plain C/C++ library or binary with no Python and no Cargo involved at all — just a CMakeLists.txt, configure.ac, or meson.build at the source root (e.g. a C library dependency that itself needs to be built from source, unresolvable_packages-style). Covers shared-library SONAME subpackage splits, %ldconfig_scriptlets, CMake/autotools/meson %build patterns, source signature verification, and %files layout for devel/runtime splits.
---

# Plain C/C++ packages

A third ecosystem, much less uniform than Python or Rust. The generator only
picks the build system and the `%autosetup -n` directory; everything else is
a placeholder to fill in from `source/` (none may stay in the spec). The shapes below are taken from real
openSUSE specs (`fmt`, `zlib-ng`, `libuv` in Factory) — verify the exact macro
spelling (grep `/usr/lib/rpm/macros.d`; the pipeline validates the spec after your turn) rather than assuming these examples
generalize perfectly to this package.

- **Shared library or not?** Grep `CMakeLists.txt` for
  `add_library(... SHARED ...)` (CMake defaults to `STATIC` unless
  `BUILD_SHARED_LIBS` is set) or `configure.ac` for `LT_INIT`/
  `AC_PROG_LIBTOOL` (libtool-managed shared libraries). If it only builds
  a binary with no installed headers, one package with no split is enough —
  skip the rest of this list. A static or header-only library that
  installs headers still needs `%package devel` (headers + cmake/`.pc`
  files); only the runtime `lib<name><soversion>` subpackage is skipped.
- **Shared library split**: a versioned shared library needs its own
  `%package -n lib<name><soversion>` (the soname's major version, e.g.
  `libfmt12`, `libz1-ng`, `libuv1` — find it from `set(SOVERSION ...)`/
  `VERSION`+`SOVERSION` in CMakeLists.txt, or `-version-info` in
  configure.ac/Makefile.am) plus a `%package devel` with
  `Requires: lib<name><soversion> = %{version}`. The base `Name:` package
  can stay empty (metapackage) or hold a CLI binary if there is one.
  **No SOVERSION set is NOT a reason to drop the split.** Installed
  headers (`.h`/`.hpp`) and `lib/cmake/` / `.pc` files must always go in a
  `-devel` package, or rpmlint's `devel-file-in-non-devel-package`
  (Badness 50 per header) blows the 999 budget (170 headers = Badness
  8500). Never "simplify"
  to one package after a link error; fix the link error and keep
  `%package devel`. If the library has no SOVERSION, name the runtime
  package `lib<name>-<version-major>_<minor>` or keep the `.so` in the base
  package and put only headers/cmake/pkgconfig in `-devel`
  (`Requires: %{name} = %{version}`).
- **Follow the SONAME upstream sets; never pin an old one.** When a new
  release changes `SOVERSION` (or the major of `VERSION`), the runtime
  subpackage is renamed to the new `lib<name><soversion>` and `%files` lists
  that soname. Do NOT keep the old name by editing `SOVERSION` with `sed`
  or a patch (`SOVERSION 0unstable`, `.so.1.0.0` hard-coded in a compat
  CMake module): it hides an ABI change from every dependent. In `%files`
  list the soname with its major and glob the rest
  (`%{_libdir}/lib<name>.so.<soversion>*`, e.g. `libfmt.so.12*`), not
  `*.so.*`; define `%global sover <soversion>` yourself at the top if you want
  a macro for it (there is no predefined one).
- **A header-only library is one noarch `-devel` package.** No runtime
  subpackage, no empty base package: `BuildArch: noarch`, `%files` for the
  headers only. If the CMake config lands in `%{_libdir}`, install it to
  `%{_datadir}` (`-DCMAKE_INSTALL_LIBDIR=share` or the project's own cmake
  dir variable) so the package stays noarch.
- **Keep the `Source:` URL when updating.** `Source0: <https url>#/%{name}-%{version}.tar.gz`
  stays an address OBS can download, not a bare file name with a sha256
  comment; and a `_service` (`tar_scm`, `set_version`) the package has stays
  as it is, with only its `revision` moved to the new tag.
- **Do not lean on a bundled copy when the distro has it.** If the old
  recipe removed a bundled library (`rm -r 3rd_party/...`) and
  `BuildRequires` the system one, keep doing that on an update.
- **Install dirs named after a version the CMake script computes**
  (e.g. `include/<name>-1.3.0/` while the tag is 1.3.1 — the project parses
  its own header, not the tag): list `%{buildroot}` after `%install` and use
  `%{_includedir}/<name>-*/` globs in `%files devel` rather than
  `%{version}`.
- **Package-private directories must be owned, exactly once.** The build
  fails late (after `%install`, in `50-check-filelist`) with
  `<pkg>.rpm: directories not owned by a package: - /usr/lib64/<name>-X.Y`
  when `%files` lists files *inside* a directory that nothing owns — a
  file entry claims the file, not its parent: a library installing
  `lib<name>.so` under `%{_libdir}/<name>-1.3.0/` and listing only
  `%{_libdir}/<name>-*/lib<name>.so` leaves the directory orphaned. Fix: add
  `%dir %{_libdir}/<name>-*` to the package that owns the files in it (or
  list the whole directory `%{_libdir}/<name>-*/` with no `%dir`). The
  opposite mistake is `RPM build warnings: File listed twice: <dir>` — a
  directory listed as a whole (`%{_includedir}/foo-*/`) plus an extra
  `%dir` line for it, or the same path in two `%files` sections. Pick one
  form per directory. A directory shared by runtime and `-devel` (e.g. the
  `.so` in the runtime package and headers/cmake in `-devel` under one
  versioned dir) is owned by the package it is *not* a leaf-only part of:
  give the `%dir` to the runtime package and list only files in `-devel`
  — never `%dir` the same path in both. After a build, compare the
  directories the build log lists under the buildroot with your `%files` to
  confirm every non-system directory is claimed once. The build log's `check-filelist`
  and `File listed twice` lines are the authoritative list; read them
  before editing `%files`.
- **`%ldconfig_scriptlets`**: every shared-library subpackage needs
  `%ldconfig_scriptlets -n lib<name><soversion>` (expands to the
  `%post`/`%postun -p /sbin/ldconfig` pair) directly before its `%files`
  section. Do not hand-write the ldconfig calls — the macro is the current
  idiom (do not invent macros; this one is real, confirmed on the build
  host's `/usr/lib/rpm/macros.d/macros.ldconfig`).

  **DO NOT rewrite `%ldconfig_scriptlets` to manual `%post -n <lib> -p
  /sbin/ldconfig` / `%postun -n <lib> -p /sbin/ldconfig` just because
  the macro looks undefined when you grep the local macro files, or the pipeline's spec check warns about it.** That
  warning is a host-macro-vs-target-macro mismatch (the local macros come
  from the review host, not an openSUSE build worker) — the macro IS defined on every openSUSE
  build worker, and the manual rewrite is strictly worse because it
  loses the single-source-of-truth property of the macro (if openSUSE
  ever changes what ldconfig invocation to use, every manual spec has
  to be hand-updated, while every `%ldconfig_scriptlets` user picks it
  up for free). The macro is not deprecated.
- **CMake `%build`/`%install`**: `%cmake` (configure) then `%cmake_build`,
  then `%cmake_install`. Never hand-write `mkdir build && cd build && cmake
  ..` — the macros already do the out-of-tree build and set the standard
  flags (`CMAKE_INSTALL_PREFIX`, `CMAKE_BUILD_TYPE`, etc.).
- **CMake `BUILD_*` knob discipline for library packages**: a typical
  upstream library CMakeLists.txt has 3–8 orthogonal `BUILD_<THING>` and
  `USE_<THING>` options that default ON for standalone development. For a
  distro library spec you almost always want the opposite defaults —
  grep `CMakeLists.txt` for `option(BUILD_` and `option(USE_` early and
  decide ALL of them in one pass:
    * `BUILD_SHARED_LIBS=ON` — the whole point of the shared-library split.
    * `BUILD_STATIC_LIBS=OFF` — openSUSE's `%cmake` already defaults this
      way; set it explicitly if the upstream default is ON.
    * `BUILD_TESTING=ON` with `%ctest` is the default: the tests are the only
      proof the library works. Turn it OFF only with a quoted build-log line
      (or a `Grep` hit in `source/`) proving no test can run here, e.g. every
      test needs a fixture from a never-fetched submodule; when only some
      tests cannot run, exclude those with `%ctest --exclude-regex
      '<test1>|<test2>'` and a comment naming the reason.
    * `BUILD_EXAMPLES=OFF` / `BUILD_DEMOS=OFF` — demo apps install to
      `/usr/bin/` under short benign names (`helloworld`, `benchmark`,
      `client`, `websocket_server`, …) and immediately produce an
      "Installed (but unpackaged)" error AND can collide with system
      binaries. Disable them unless you deliberately want an examples
      subpackage.
    * `USE_SUBMODULE=OFF` (or whatever the vendor-vs-system knob is
      called) — openSUSE strongly prefers system libraries. If this is on,
      the vendored submodule needs to be in the archive AND the system
      package must be absent — rarely the right choice. Pair with
      the `BUILD_TESTING` rule of the previous bullet.
    * Opt-in tool subpackages: a code-gen or CLI tool (protobuf's `protoc`,
      gRPC's `grpc_cpp_plugin`) is a
      conscious decision — EITHER package it as `-n <name>-tools` /
      `-n <name>-ctl` with its own `%files` section AND leave the
      corresponding `BUILD_<TOOL>` ON, OR drop it entirely with
      `BUILD_<TOOL>=OFF`. Pick one upfront and keep it.
  Decide all of them in one pass by reading `CMakeLists.txt`'s `option(...)`
  lines directly: each wrong knob costs a full OBS build.
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
  neither exists (Grep `source/` for `add_test`/`check:` finds nothing), say
  so and leave `%check` out with a plain `# no upstream test suite` comment
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
  name; verify it. If the dep isn't in OBS yet and has to be reported in
  `unresolvable_packages`, strip the `lib`-prefix and `-devel`/`-dev` suffix
  first — `libtrantor-devel` is reported as `trantor` — because the
  resolver searches PyPI and GitHub for the upstream project name, which
  nobody writes with the distro's devel-package wrapping (see convention
  27's C/C++ bullet for the full reasoning).
- **`%files`**: `%license` for the actual license file in `source/`
  (`LICENSE`, `COPYING`, `COPYING.LESSER`), `%doc` for `README`/`AUTHORS`/
  `ChangeLog` if present, `%{_includedir}/...` and
  `%{_libdir}/pkgconfig/*.pc` in `%files devel`, and `%{_libdir}/lib<name>.so.*`
  (the versioned SONAME symlink, not the unversioned `.so` — that one goes
  in `%files devel` since it's only needed at link time) in the shared
  library subpackage.
- **GObject-Introspection typelibs**: a C/C++ library that ships `.gir`/
  `.typelib` files (common for anything in the GNOME ecosystem — grep for
  `gobject-introspection` in `meson.build`/`configure.ac`, or
  `-Dintrospection=enabled` in reference specs) needs
  `BuildRequires: gobject-introspection-devel`, a `-Dintrospection=enabled`
  (or equivalent) build flag, and a standalone subpackage named
  `typelib-<ApiMajor>_<ApiMinor>-<Namespace>-<ApiVersion>` holding only
  `%{_libdir}/girepository-1.0/<Namespace>-<ApiVersion>.typelib` (openSUSE
  naming convention). The `.gir` XML goes in `-devel`
  (`%{_datadir}/gir-1.0/*.gir`), the compiled `.typelib` in the standalone
  package; `-devel` adds `Requires: typelib-...` so pulling the devel
  package drags in the typelib automatically. Real example: `gstreamer`.
- **File capabilities (POSIX caps) on a helper binary**: when a helper
  needs `cap_net_bind_service`, `cap_sys_time`, etc. (upstream build
  options like `-Dptp-helper-permissions=capabilities` are the giveaway),
  openSUSE's `permissions` package applies them from
  `/etc/permissions.d/<name>` at install time:
    Requires(pre):  permissions
    %verifyscript
    %verify_permissions -e %{_libexecdir}/<path>/<helper>
    %post
    %set_permissions %{_libexecdir}/<path>/<helper>
    %files
    %verify(not mode caps) %{_libexecdir}/<path>/<helper>
  The `%verify(not mode caps)` on the `%files` line suppresses rpm's
  verify-time complaints about the setcap'd file. Real example:
  `gstreamer`'s `gst-ptp-helper`.
- **Source signature verification**: when the release page publishes a
  `.sig`/`.asc` next to the tarball, add it and the keyring as sources
  (convention 93; real examples: `acl`, `attr`, `coreutils`, `bash`,
  `elfutils`). OBS verifies the signature against the keyring automatically.

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

## Git submodules and network access in `%prep`/`%build`

OBS builds have no network. GitHub's release archives also omit git
submodules, so a CMake project that vendors dependencies as submodules
(a `third_party/` of submodules, llama.cpp-style trees) arrives with those
directories empty. Do NOT "fix" this with `git submodule update --init`,
`git clone`, `git config user.email` or a `git commit` in `%prep`: there is
no network and nothing to fetch from. Instead,
in order of preference:
1. turn the vendored dependency off and use the distro's `-devel` package
   (`-DWITH_MKL=OFF`, `-DOPENMP_RUNTIME=COMP`, `-DBUILD_CLI=OFF`,
   `-DUSE_SYSTEM_<NAME>=ON` / `-DCMAKE_DISABLE_FIND_PACKAGE_...` — read the
   project's CMake options), adding the matching `BuildRequires`;
2. add the submodule's own release tarball as an extra `SourceN:` (declared
   as a full-URL `Source` fetched by a `_service` with the `download_files`
   service in `mode="manual"`, convention 92) and unpack it into the empty directory in
   `%prep` with `tar -xf %{SOURCE1} --strip-components=1 -C third_party/<dir>`;
3. if neither is possible, report the dependency in `unresolvable_packages`
   (convention 27) rather than inventing a source.

## Known failures with one exact fix

The pipeline recognises these in the build log (`log/cause.rs`) and puts the
recipe first in the turn; apply it instead of returning `blocked`.

- `Cython 3.3 requires the Python Limited API version to be 3.9 or greater`
  (scikit-build-core `wheel.py-api = "cp38"`): in `%prep`,
  `sed -i -E 's/(py-api[[:space:]]*=[[:space:]]*")cp3[0-8]"/\1cp39"/' pyproject.toml`.
- A CMake licence guard (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION`): a fact
  to check, convention 78; never the S3 download
  option, never `blocked`.
- CUDA/NVIDIA failures (split `nvidia-cutlass-dsl-libs-*` wheels, a CUDA
  wheel that cannot import without a device): skill nvidia-wheels.
