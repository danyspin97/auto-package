//! The skeletons of new specs: the wheel, the binary wheel, the monorepo and the C
//! project skeletons, and the flavor version.

use super::*;
use crate::spec::license::*;
use crate::spec::requires_exclude::*;
use crate::spec::rpmlintrc::*;
use crate::spec::siblings::*;
use crate::spec::wheel::*;

/// [`wheel_binary_skeleton`] for a release whose wheels are each locked to one
/// CPython (convention 49's Triton shape): one `SourceN:` per flavor and one
/// hardcoded `%python<NNN>_pyproject_install` per flavor, since the shared
/// `%pyproject_install` repeats a single argument for every flavor.
///
/// `wheels` is `(flavor, wheel filename)` — e.g. `("python313", "x-cp313-...whl")`,
/// in flavor order.
pub fn wheel_multi_flavor_skeleton(
    pkg_name: &str,
    version: &str,
    wheels: &[(String, String)],
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    wheel_multi_flavor_skeleton_shared(
        pkg_name,
        version,
        wheels,
        release,
        root_is_purelib,
        layout,
        &[],
    )
}

/// [`wheel_multi_flavor_skeleton`] for a project whose other wheels fill the
/// same directories (see [`wheel_binary_skeleton_shared`]).
pub fn wheel_multi_flavor_skeleton_shared(
    pkg_name: &str,
    version: &str,
    wheels: &[(String, String)],
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    siblings: &[SiblingWheel],
) -> String {
    let Some((_, first)) = wheels.first() else {
        return String::new();
    };
    let single = wheel_binary_skeleton_shared(
        pkg_name,
        version,
        first,
        release,
        root_is_purelib,
        layout,
        siblings,
    );
    let sources: String = wheels
        .iter()
        .enumerate()
        .map(|(i, (_, file))| format!("Source{i}:        {file}\n"))
        .collect();
    let installs: String = wheels
        .iter()
        .enumerate()
        .map(|(i, (flavor, _))| format!("%{flavor}_pyproject_install %{{SOURCE{i}}}\n"))
        .collect();
    single
        .replacen(&format!("Source0:        {first}\n"), &sources, 1)
        .replacen("%pyproject_install %{SOURCE0}\n", &installs, 1)
}

/// Whether the values a skeleton writes unescaped into its header (`Name:`,
/// `Version:`, `Source:`/`Source0:`) are plain. They come from the command line
/// and from PyPI's JSON, which this code does not check, and a value with a
/// space, `%` or newline would rewrite the rest of the spec. A plain name is
/// [`is_plain_dependency_name`]; a version is PEP 440's characters; a wheel
/// filename is the same plain set.
fn skeleton_header_values_are_plain(pkg_name: &str, version: &str, wheel: Option<&str>) -> bool {
    let plain_text = |s: &str| {
        s.len() <= 255
            && s.starts_with(|c: char| c.is_ascii_alphanumeric())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '!'))
    };
    is_plain_dependency_name(pkg_name) && plain_text(version) && wheel.is_none_or(plain_text)
}

/// Generate a minimal skeleton spec for a monorepo package.
///
/// py2pack only understands single-project PyPI packages; for a monorepo with
/// multiple subprojects it would produce a spec for one subproject only,
/// leaving all siblings out. This skeleton carries the correct header fields
/// from the PyPI release and leaves %build/%install/%check empty so the review
/// agent can fill in the full multi-subpackage layout per convention 24.
pub fn monorepo_skeleton(
    pkg_name: &str,
    version: &str,
    subprojects: &[ap_ecosystem::sdist::Subproject],
    release: Option<&ap_ecosystem::pypi::Release>,
) -> String {
    if !skeleton_header_values_are_plain(pkg_name, version, None) {
        return String::new();
    }
    let name = pkg_name.to_lowercase();
    let summary = rpm_text(
        release
            .and_then(|r| r.summary.as_deref())
            .unwrap_or("FIXME: add package summary"),
    );
    let license = rpm_text(
        &release
            .map(|r| license_tag(&r.license))
            .unwrap_or_else(|| "FIXME".to_string()),
    );
    let url = rpm_text(
        release
            .and_then(|r| r.homepage.as_deref())
            .unwrap_or("FIXME"),
    );

    let sub_list: String = subprojects
        .iter()
        .filter(|s| {
            ap_ecosystem::sdist::is_safe_manifest_dir(&s.dir)
                && s.name
                    .as_deref()
                    .is_none_or(ap_ecosystem::sdist::is_safe_top_level_dir)
        })
        .map(|s| {
            format!(
                "# TODO: add %package -n python-%{{name}}-{sub} for {dir}/\n",
                sub = s.name.as_deref().unwrap_or(&s.dir),
                dir = s.dir,
            )
        })
        .collect();

    format!(
        "#\n\
         # spec file for package python-{name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         %{{?sle15_python_module_pythons}}\n\
         Name:           python-{name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        {summary}\n\
         License:        {license}\n\
         URL:            {url}\n\
         Source:         {name}-{version}.tar.gz\n\
         BuildRequires:  python-rpm-macros\n\
         BuildRequires:  %{{python_module pip}}\n\
         BuildRequires:  fdupes\n\
         %python_subpackages\n\
         \n\
         # This is a monorepo: all subprojects below must become %%package sections.\n\
         # Convention 24 governs the layout — do not build only one subproject.\n\
         {sub_list}\
         %description\n\
         {summary}\n\
         \n\
         %prep\n\
         %autosetup -p1 -n {name}-{version}\n\
         \n\
         %build\n\
         # TODO: build each subproject in dependency order\n\
         \n\
         %install\n\
         # TODO: install each subproject\n\
         \n\
         %check\n\
         # TODO: check each subproject\n\
         \n\
         %files %{{python_files}}\n\
         %license LICENSE\n\
         \n\
         %changelog\n",
        name = name,
        version = version,
        summary = summary,
        license = license,
        url = url,
        sub_list = sub_list,
    )
}
mod binary;
pub use binary::*;

/// The `(major, minor)` of a Python flavor name: `python313` -> (3, 13),
/// `python39` -> (3, 9), `python314t` -> (3, 14).
pub(super) fn flavor_version(flavor: &str) -> Option<(u32, u32)> {
    let rest = flavor.strip_prefix("python")?.trim_end_matches('t');
    let (major, minor) = rest.split_at_checked(1)?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Which native build system a plain C/C++ project uses. Decided by
/// `ap_ecosystem::sdist::SdistInfo::cmake_manifest`/`autotools_manifest`.
pub enum CBuildSystem {
    Cmake,
    Meson,
    Autotools,
}

/// Build a skeleton spec for a plain C/C++ project: no Python or Cargo
/// packaging anywhere in the archive, just a native build system. This is a
/// third ecosystem for the generator (Python and Rust are the other two), and
/// a much less uniform one — whether the project ships a shared library
/// needing a `lib<name><soversion>`/`-devel` split, and which `-devel`/
/// `pkgconfig()` BuildRequires its `find_package()`/`pkg_check_modules()`
/// calls need, both require reading `CMakeLists.txt`/`configure.ac` and
/// `source/` directly. The skeleton stays deliberately bare; see convention 32 (section 33).
pub fn c_project_skeleton(
    pkg_name: &str,
    version: &str,
    url: &str,
    top_level_dir: &str,
    build: CBuildSystem,
    // The subdirectory of the top-level directory the build file is in.
    subdir: Option<&str>,
) -> String {
    let name = pkg_name.to_lowercase();
    let (build_kind, build_requires, build_section, install_section): (&str, &str, &str, &str) =
        match build {
            CBuildSystem::Cmake => (
                "CMake",
                "BuildRequires:  cmake\n\
             BuildRequires:  gcc-c++\n\
             BuildRequires:  pkg-config\n",
                "%cmake\n\
             %cmake_build\n",
                "%cmake_install\n",
            ),
            CBuildSystem::Meson => (
                "meson",
                "BuildRequires:  meson\n\
             BuildRequires:  gcc-c++\n\
             BuildRequires:  pkg-config\n",
                "%meson\n\
             %meson_build\n",
                "%meson_install\n",
            ),
            CBuildSystem::Autotools => (
                "autotools",
                "BuildRequires:  autoconf\n\
             BuildRequires:  automake\n\
             BuildRequires:  libtool\n\
             BuildRequires:  gcc-c++\n\
             BuildRequires:  pkg-config\n",
                // A release archive is a repository snapshot, not a `make dist`
                // tarball — it has configure.ac/Makefile.am but no generated
                // `configure` script yet (see libuv.spec's `./autogen.sh`, the
                // real-world precedent this mirrors).
                "autoreconf -fiv\n\
             %configure\n\
             %make_build\n",
                "%make_install\n\
             find %{buildroot} -type f -name \"*.la\" -delete -print\n",
            ),
        };
    // Each section starts in the top-level directory.
    let (build_section, install_section) = match subdir {
        Some(dir) => (
            format!("cd {dir}\n{build_section}"),
            format!("cd {dir}\n{install_section}"),
        ),
        None => (build_section.to_string(), install_section.to_string()),
    };

    format!(
        "#\n\
         # spec file for package {name}\n\
         #\n\
         # Copyright (c) 2026 SUSE LLC\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         \n\
         \n\
         Name:           {name}\n\
         Version:        {version}\n\
         Release:        0\n\
         Summary:        TODO: fill in summary\n\
         License:        TODO: fill in SPDX license - read LICENSE/COPYING in source/\n\
         URL:            {url}\n\
         Source0:        %{{name}}-%{{version}}.tar.gz\n\
         {build_requires}\
         \n\
         %description\n\
         TODO: fill in description\n\
         \n\
         # No Python or Cargo packaging found anywhere in this archive - a plain\n\
         # C/C++ project built with {build_kind}. See convention 32 (section 33): decide whether\n\
         # this ships a shared library needing a lib%%{{name}}<soversion>/%%{{name}}-devel\n\
         # split, and find the -devel/pkgconfig() BuildRequires the build actually needs.\n\
         # Add no ldconfig scriptlets: a file trigger runs ldconfig (convention 104).\n\
         \n\
         %prep\n\
         %autosetup -n {top_level_dir}\n\
         \n\
         %build\n\
         {build_section}\
         \n\
         %install\n\
         {install_section}\
         \n\
         %check\n\
         # TODO: run the test suite if there is one\n\
         \n\
         %files\n\
         # TODO: list the files %%install put in the buildroot - convention 15's\n\
         # \"no globs\" rule applies here too.\n\
         %license TODO\n\
         \n\
         %changelog\n",
        name = name,
        version = version,
        url = url,
        build_requires = build_requires,
        build_kind = build_kind,
        top_level_dir = top_level_dir,
        build_section = build_section,
        install_section = install_section,
    )
}
