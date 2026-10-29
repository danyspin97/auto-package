//! The skeletons of new specs: the wheel, the binary wheel, the monorepo and the C
//! project skeletons, and the flavor version.

use super::*;
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

pub fn wheel_binary_skeleton(
    pkg_name: &str,
    version: &str,
    wheel_filename: &str,
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
) -> String {
    wheel_binary_skeleton_shared(
        pkg_name,
        version,
        wheel_filename,
        release,
        root_is_purelib,
        layout,
        &[],
    )
}

/// [`wheel_binary_skeleton`] for a wheel whose project has other wheels filling
/// the same directories (convention 83). `siblings` are the other wheels of the
/// project (see [`sibling_wheels`]; build order does not matter): directories
/// they also hold are co-owned with `%dir` and this wheel's own files listed one
/// by one, and a sibling that owns a package (`__init__.py`) this wheel adds
/// files to becomes a `Requires:`, so the import chain is complete. Nothing in a
/// sibling's `RECORD` or payload is touched.
///
/// The install directory follows the wheel's own `Root-Is-Purelib`, with one
/// exception: a wheel that says `false` but shares a top-level directory with a
/// sibling that says `true` moves its tree into `%{python_sitelib}` in
/// `%install`, because a package tree split between `lib` and `lib64` cannot be
/// imported (a regular package in one never sees the other) and the pure
/// sibling, being noarch, cannot go anywhere else.
pub fn wheel_binary_skeleton_shared(
    pkg_name: &str,
    version: &str,
    wheel_filename: &str,
    release: &Release,
    root_is_purelib: Option<bool>,
    layout: Option<&ap_ecosystem::sdist::WheelLayout>,
    siblings: &[SiblingWheel],
) -> String {
    if !skeleton_header_values_are_plain(pkg_name, version, Some(wheel_filename)) {
        return String::new();
    }
    let foreign: Vec<String> = siblings
        .iter()
        .flat_map(|s| s.files.iter().cloned())
        .collect();
    let name = pkg_name.to_lowercase();
    let summary = release
        .summary
        .as_deref()
        .unwrap_or("FIXME: add package summary");
    // rpm expands macros in `Summary:` and `URL:`: a `%` of upstream's text is
    // doubled (`wheel_description` does the same for the description).
    let summary_tag = rpm_text(summary);
    // The release's own licence, or, when PyPI has none the tool can use, what the
    // wheel's METADATA declares (a `License: FIXME` is a turn the review spends
    // reading the wheel for what the generator already read).
    let license = wheel_declared_license(release, layout);
    let license = match layout {
        Some(l) => with_license_terms(license, &wheel_vendor_license_terms(l)),
        None => license,
    };
    // A licence is rpm text too: its `%` is doubled and it stays on one line.
    let license_text = rpm_text(&license);
    let description = wheel_description(layout.and_then(|l| l.description.as_deref()), summary);
    let url = rpm_text(release.homepage.as_deref().unwrap_or("FIXME"));
    // Two independent facts about a wheel:
    //  * is the PACKAGE arch-specific? Only the platform tag says so:
    //    `-none-any.whl` is noarch; anything else (`manylinux_*_x86_64`) is not,
    //    and gets `ExclusiveArch` for the architecture it was built for.
    //  * which DIRECTORY does it install into? That is `Root-Is-Purelib` in the
    //    wheel's WHEEL file (`true` -> %{python_sitelib}, `false` ->
    //    %{python_sitearch}), whatever the tag. NVIDIA's CUDA wheels are
    //    manylinux-tagged but purelib, so a spec that assumes sitearch for them
    //    packages nothing and `%check` cannot import the module.
    let is_pure = wheel_filename.ends_with("-none-any.whl");
    let arch_line = if is_pure {
        "BuildArch:      noarch\n".to_string()
    } else if wheel_filename.contains("x86_64") {
        "ExclusiveArch:  x86_64\n".to_string()
    } else if wheel_filename.contains("aarch64") {
        "ExclusiveArch:  aarch64\n".to_string()
    } else {
        String::new()
    };
    // No `WHEEL` verdict: the platform tag decides, and any tag but `-none-any`
    // (compiled code, convention 83 b) means `%{python_sitearch}`.
    let declared_purelib = root_is_purelib.unwrap_or(is_pure);
    let purelib = declared_purelib || layout.is_some_and(|l| sibling_forces_purelib(l, siblings));
    let sitedir = if purelib {
        "python_sitelib"
    } else {
        "python_sitearch"
    };
    let source99_line = if wheel_needs_rpmlintrc(wheel_filename, layout) {
        format!("Source99:       python-{name}.rpmlintrc\n")
    } else {
        String::new()
    };
    // A `%check` that works the first time. There is no source tree to test, so
    // either import the top-level package, or — for namespace-only/binary-only
    // wheels (NVIDIA's `nvidia/` holding just `lib/*.so`) — assert that the
    // payload landed where `%files` will look. Importing needs the buildroot on
    // PYTHONPATH (the build directory is not on `sys.path`), and everything runs
    // inside `%python_expand` so each flavor gets its own interpreter and path.
    let check_block = match layout {
        Some(l) if l.importable.as_deref().is_some_and(ap_ecosystem::sdist::is_python_identifier) => format!(
            "%check\n%python_expand PYTHONPATH=%{{buildroot}}%{{${sitedir}}} $python -c \"import {}\"\n",
            l.importable.as_deref().unwrap_or_default()
        ),
        // A PEP 420 namespace wheel (`cuda/bindings/`): nothing to `import` at
        // the top, and the regular package below it may need a device library
        // at import. The spec is located with `PathFinder`, one dotted prefix at
        // a time and in the buildroot only: no package code runs, and a regular
        // package of the same name that a sibling wheel installed (nccl-
        // extensions' `nccl/ep/` beside nccl4py's `nccl/__init__.py`) cannot hide
        // this wheel's files, which `importlib.util.find_spec` lets it do.
        Some(l) if l.namespace_module.is_some() => format!(
            "%check\n%python_expand PYTHONPATH=%{{buildroot}}%{{${sitedir}}} $python -c \"{}\"\n",
            namespace_find_spec_snippet(
                l.namespace_module.as_deref().unwrap_or_default(),
                &format!("%{{buildroot}}%{{${sitedir}}}")
            )
        ),
        Some(l) if l.first_shared_lib.as_deref().is_some_and(ap_ecosystem::sdist::is_shell_safe_member) => format!(
            "%check\n%python_expand test -e %{{buildroot}}%{{${sitedir}}}/{}\n",
            l.first_shared_lib.as_deref().unwrap_or_default()
        ),
        // Executables only (`bin/nvdisasm`), a header library, a `.pth`: the
        // payload is the shortest file `%install` leaves.
        Some(l) if wheel_payload_probe(l).is_some() => format!(
            "%check\n%python_expand test -e %{{buildroot}}%{{${sitedir}}}/{}\n",
            wheel_payload_probe(l).unwrap_or_default()
        ),
        // A metapackage wheel (`cuda-toolkit`) installs only its dist-info.
        Some(l) if l.dist_info.as_deref().is_some_and(ap_ecosystem::sdist::is_shell_safe_member) && !l.files.is_empty() => format!(
            "%check\n%python_expand test -d %{{buildroot}}%{{${sitedir}}}/{}\n",
            l.dist_info.as_deref().unwrap_or_default()
        ),
        _ => "%check\n# TODO: smoke-test the import name from source/*.dist-info/METADATA:\n\
              # %python_expand PYTHONPATH=%{buildroot}%{$python_sitelib} $python -c \"import <name>\"\n"
            .to_string(),
    };
    // What rpmlint would otherwise be told to ignore is fixed here: the wheel's
    // libraries lose their missing executable bit, and the bundled C headers
    // (not needed at run time) are removed.
    let mut install_extra = String::new();
    if let (true, false, Some(l)) = (purelib, declared_purelib, layout) {
        install_extra.push_str(&sitelib_relocation(l));
    }
    if layout.is_some_and(|l| l.first_shared_lib.is_some()) {
        install_extra.push_str(&format!(
            "# the wheel ships its libraries without the executable bit\n\
             %python_expand find %{{buildroot}}%{{${sitedir}}} -name '*.so*' -type f -exec chmod 0755 {{}} +\n"
        ));
    }
    // Upstream wheels carry the executable bit on files that are not programs
    // (a Python module, a stylesheet): rpmlint's `script-without-shebang`, one
    // error per file. Only an ELF file or a script with a `#!` line keeps it.
    let mode_fix = format!(
        "# files without a shebang or ELF header do not stay executable\n\
         %python_expand find %{{buildroot}}%{{${sitedir}}} -type f -perm /111 -exec sh -c 'for f; do head -c4 \"$f\" | grep -aqE \"^#!|ELF\" || chmod 0644 \"$f\"; done' sh {{}} +\n"
    );
    if layout.is_some_and(|l| !l.header_dirs.is_empty() && wheel_header_cleanups(l).is_empty()) {
        install_extra.push_str(
            "# the headers are this wheel's payload, so they are kept; they belong in a\n\
             # -devel package rather than being removed\n",
        );
    }
    let cleanups = layout.map(wheel_header_cleanups).unwrap_or_default();
    if !cleanups.is_empty() {
        install_extra.push_str("# bundled headers are not needed at run time\n");
    }
    for cleanup in cleanups {
        install_extra.push_str(&match cleanup {
            HeaderCleanup::Dir(dir) => format!(
                "%python_expand rm -rf %{{buildroot}}%{{${sitedir}}}/{dir}\n"
            ),
            HeaderCleanup::Headers(dir) => format!(
                "%python_expand find %{{buildroot}}%{{${sitedir}}}/{dir} -type f \\( -name '*.h' -o -name '*.hpp' \\) -delete\n"
            ),
        });
    }

    let (alt_requires, alt_clone, alt_scriptlets, alt_files) =
        wheel_alternatives_fragments(layout.map_or(&[][..], |l| l.console_scripts.as_slice()));
    let requires_exclude = wheel_requires_exclude_line(layout, is_pure);

    let files_block = layout
        .and_then(|l| wheel_files_entries(l, sitedir, &foreign))
        .unwrap_or_else(|| {
            "# TODO: list the exact paths pyproject_install put in the buildroot\n\
             # (see source/ for the unpacked wheel's real layout); globs are not allowed\n\
             # here.\n"
                .to_string()
        });

    let spec = format!(
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
         Summary:        {summary_tag}\n\
         License:        {license_text}\n\
         URL:            {url}\n\
         Source0:        {wheel_filename}\n\
         {source99_line}\
         {arch_line}\
         BuildRequires:  python-rpm-macros\n\
         BuildRequires:  %{{python_module pip}}\n\
         BuildRequires:  fdupes\n\
         Requires:       python-base\n\
         {alt_requires}\
         {requires_exclude}\
         %python_subpackages\n\
         \n\
         %description\n\
         {description}\n\
         \n\
         %prep\n\
         # nothing to unpack here: the source is a prebuilt wheel\n\
         \n\
         %build\n\
         # prebuilt binary wheel - nothing to compile\n\
         \n\
         %install\n\
         %pyproject_install %{{SOURCE0}}\n\
         {install_extra}\
         {alt_clone}\
         {mode_fix}\
         %python_expand %fdupes %{{buildroot}}/%{{${sitedir}}}\n\
         \n\
         {check_block}\
         \n\
         {alt_scriptlets}\
         %files %{{python_files}}\n\
         {files_block}\
         {alt_files}\
         \n\
         %changelog\n",
        name = name,
        version = version,
        summary_tag = summary_tag,
        description = description,
        license_text = license_text,
        url = url,
        wheel_filename = wheel_filename,
        arch_line = arch_line,
        sitedir = sitedir,
        check_block = check_block,
        files_block = files_block,
    );
    // The siblings that own a package this wheel adds files to.
    let mut spec = spec;
    if let Some(l) = layout {
        let mut lines = String::new();
        for owner in shared_package_owners(l, siblings) {
            if !spec
                .lines()
                .any(|x| x.trim_end() == format!("Requires:       {owner}"))
            {
                lines.push_str(&format!("Requires:       {owner}\n"));
            }
        }
        if !lines.is_empty() {
            spec = spec.replacen(
                "Requires:       python-base\n",
                &format!("Requires:       python-base\n{lines}"),
                1,
            );
        }
    }
    // What the wheel's own metadata requires at run time.
    match layout {
        Some(l) => add_runtime_dependencies(&spec, &l.requires_dist, pkg_name),
        None => spec,
    }
}

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
    Autotools,
}

/// Build a skeleton spec for a plain C/C++ project: no Python or Cargo
/// packaging anywhere in the archive, just a native build system. This is a
/// third ecosystem for the generator (Python and Rust are the other two), and
/// a much less uniform one — whether the project ships a shared library
/// needing a `lib<name><soversion>`/`-devel` split, and which `-devel`/
/// `pkgconfig()` BuildRequires its `find_package()`/`pkg_check_modules()`
/// calls need, both require reading `CMakeLists.txt`/`configure.ac` and
/// `source/` directly. The skeleton stays deliberately bare; see convention 29.
pub fn c_project_skeleton(
    pkg_name: &str,
    version: &str,
    url: &str,
    top_level_dir: &str,
    build: CBuildSystem,
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
         # C/C++ project built with {build_kind}. See convention 29: decide whether\n\
         # this ships a shared library needing a lib%%{{name}}<soversion>/%%{{name}}-devel\n\
         # split, find the -devel/pkgconfig() BuildRequires the build actually needs,\n\
         # and add %%ldconfig_scriptlets to any shared-library subpackage.\n\
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
