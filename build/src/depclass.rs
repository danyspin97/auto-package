//! Classification of the dependency names a build or an agent reports as
//! missing.
//!
//! What gets reported is not always a package: OBS names RPM capabilities
//! (`libcuda.so.1()(64bit)`, `pkgconfig(zlib)`, `cmake(Foo)`, `/usr/bin/foo`,
//! `python3dist(foo)`), the agent names `python313-foo` or `libfoo-devel`,
//! and some of it is neither something to build nor a PyPI project. Every
//! reported name goes through [`triage`] once, so the pipeline and the
//! dependency queue agree on what a name is and say why a name was dropped.
//! Where a package already is (distro, linked project, own project) is
//! [`availability`]. Both are pure apart from the `provides` callback.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::osc::{self, BuildStatus};

/// What a name to build most likely is, which decides how it is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    /// Named with a `python<ver>-` prefix or `python3dist(...)`: a PyPI project.
    Python,
    /// A C/C++ library or other native project: looked up in upstream repositories.
    Native,
    /// A bare name, already stripped by an earlier step: PyPI first, then upstream.
    Unknown,
}

/// Why a reported name is not something to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// The interpreter's own subpackage (`python3-devel`) or the interpreter.
    Interpreter(String),
    /// A PyPI package that only redistributes a tool the distro ships.
    SystemTool(String),
    /// A library of a vendor driver or toolkit that has no public source.
    Vendor(String),
    /// A capability that is not a package (file, perl module, rpmlib feature).
    NotAPackage {
        capability: String,
        why: &'static str,
    },
    /// The distro ships it.
    Distro(String),
    /// Nothing usable was left of the name.
    Empty,
}

impl Skip {
    /// The user-visible reason, for the log and for the message that gives up
    /// on a package waiting on the name.
    pub fn reason(&self) -> String {
        match self {
            Skip::Interpreter(n) => format!(
                "{n} is part of the Python interpreter packages, which the distro ships \
                 (BuildRequires the matching python3-* package instead of building it)"
            ),
            Skip::SystemTool(n) => format!(
                "{n} redistributes a system package of the same name \
                 (see SYSTEM_TOOL_ALIASES), which the distro already ships"
            ),
            Skip::Vendor(n) => format!(
                "{n} belongs to a vendor driver or toolkit that cannot be built here (no \
                 public source, or it needs the CUDA compiler), so it is never packaged as \
                 a `python-<lib>` package for a C library. A Python package that loads it \
                 from site-packages depends on NVIDIA's own wheel of it (`nvidia-<lib>-cuNN` \
                 on PyPI); anything else must not require it"
            ),
            Skip::NotAPackage { capability, why } => {
                format!("{capability} is not a package: {why}")
            }
            Skip::Distro(n) => format!(
                "the distro already provides it (see `zypper search --provides \
                 --match-exact {n}`)"
            ),
            Skip::Empty => "the name is empty".to_string(),
        }
    }

    /// Whether the agent should be told the name can never be built, so it
    /// stops reporting it (convention 28, exception 4). A name the distro
    /// ships is satisfied rather than out of reach.
    pub fn is_unbuildable(&self) -> bool {
        matches!(self, Skip::Vendor(_) | Skip::NotAPackage { .. })
    }
}

/// A reported name after [`triage`]: a package to look for, or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Triage {
    Build { name: String, kind: PackageKind },
    Skip(Skip),
}

/// What a reported name is by its spelling alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reported {
    Package {
        name: String,
        kind: PackageKind,
    },
    /// An RPM capability, not a package name. The distro is asked for it as
    /// spelled; when it provides nothing, `fallback` says what the name means.
    Capability {
        capability: String,
        fallback: Fallback,
    },
    Never(Skip),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    Package { name: String, kind: PackageKind },
    Unbuildable(Skip),
}

/// Stems of the interpreter's own subpackages (`python3-devel`,
/// `python-rpm-macros`, ...): the distro ships them, and no PyPI project of
/// that name is what the requirement means.
const PYTHON_INTERPRETER_STEMS: &[&str] = &[
    "base",
    "curses",
    "devel",
    "doc",
    "idle",
    "rpm-generators",
    "rpm-macros",
    "testsuite",
    "tk",
    "tools",
    "xml",
];

/// Library stems of vendor drivers and toolkits that ship no source: the
/// driver's own library (`libcuda`, the one a container gets from the host)
/// and NVIDIA's closed-source user-space libraries.
const VENDOR_LIBRARY_PREFIXES: &[&str] = &[
    "libcuda",
    "libnvidia-",
    "libnvcuvid",
    "libnvoptix",
    // The CUDA toolkit's own libraries, which need `nvcc` to build and ship
    // only as NVIDIA's binaries: never a `python-<lib>` package of their own.
    "libnccl",
    "libcudnn",
    "libcublas",
    "libcufft",
    "libcurand",
    "libcusolver",
    "libcusparse",
    "libcudart",
    "libnvrtc",
    "libnvjitlink",
    "libnvshmem",
    "libcufile",
    "libnvtx",
];

/// `python3dist(x)` / `python313dist(x)` -> `x`.
fn python_dist(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("python")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    rest.strip_prefix("dist(")?.strip_suffix(')')
}

/// `libfoo.so.1()(64bit)` / `libfoo.so.1` -> `libfoo`.
fn soname_stem(name: &str) -> Option<&str> {
    let (stem, rest) = name.split_once(".so")?;
    let plain = !stem.is_empty()
        && !stem.contains(['/', '(', ')'])
        && (rest.is_empty() || rest.starts_with('.') || rest.starts_with('('));
    plain.then_some(stem)
}

/// Whether a soname stem carries the 8-hex-digit hash auditwheel appends to the
/// libraries it bundles into a wheel (`libavcodec-f34387fd`,
/// `libopenblasp-r0-37b5f859.3.3`): such a library is never a package.
fn is_hashed_bundled_library(stem: &str) -> bool {
    stem.split('-').skip(1).any(|part| {
        let hash = part.split('.').next().unwrap_or("");
        hash.len() == 8
            && hash.chars().all(|c| c.is_ascii_hexdigit())
            && hash.chars().any(|c| c.is_ascii_digit())
    })
}

/// The names among `names` that stand for a native (C/C++) library reported in
/// `originals`: a translated name (`trantor`, from `libtrantor-devel`) loses the
/// spelling that said it was a library, and without that it would be looked up
/// on PyPI first. A name is native when the key of a native original contains
/// it or it contains that key.
pub fn native_among(originals: &[String], names: &[String]) -> Vec<String> {
    let native_keys: Vec<String> = originals
        .iter()
        .filter_map(|raw| match classify_reported(raw) {
            Reported::Package {
                name,
                kind: PackageKind::Native,
            }
            | Reported::Capability {
                fallback:
                    Fallback::Package {
                        name,
                        kind: PackageKind::Native,
                    },
                ..
            } => Some(osc::fuzzy_key(&name)),
            _ => None,
        })
        .collect();
    names
        .iter()
        .filter(|n| {
            let key = osc::fuzzy_key(n);
            !key.is_empty()
                && native_keys
                    .iter()
                    .any(|nk| nk.contains(&key) || key.contains(nk.as_str()))
        })
        .cloned()
        .collect()
}

/// What `raw` is by its spelling alone: nothing here asks the distro or OBS.
pub fn classify_reported(raw: &str) -> Reported {
    let raw = raw.trim();
    if raw.is_empty() {
        return Reported::Never(Skip::Empty);
    }
    if let Some(dist) = python_dist(raw) {
        return Reported::Package {
            name: dist.to_string(),
            kind: PackageKind::Python,
        };
    }
    let not_a_package = |why| Reported::Capability {
        capability: raw.to_string(),
        fallback: Fallback::Unbuildable(Skip::NotAPackage {
            capability: raw.to_string(),
            why,
        }),
    };
    if raw.starts_with('/') {
        return not_a_package(
            "a file requirement: BuildRequires the package that ships the file \
             (or the tool's own name) instead",
        );
    }
    for prefix in ["rpmlib(", "config(", "perl(", "group(", "user("] {
        if raw.starts_with(prefix) {
            return not_a_package("an RPM feature or a perl module the distro has to provide");
        }
    }
    for prefix in ["pkgconfig(", "cmake("] {
        if let Some(inner) = raw
            .strip_prefix(prefix)
            .and_then(|r| r.strip_suffix(')'))
            .filter(|i| !i.is_empty())
        {
            return Reported::Capability {
                capability: raw.to_string(),
                fallback: Fallback::Package {
                    name: inner.to_string(),
                    kind: PackageKind::Native,
                },
            };
        }
    }
    if let Some(stem) = soname_stem(raw) {
        // `--nvidia-redist`: the CUDA libraries are packages of their own,
        // `cuda-<component>`, whose subpackages carry the soname.
        if let Some(package) = crate::redist::package_name(raw) {
            return Reported::Capability {
                capability: raw.to_string(),
                fallback: Fallback::Package {
                    name: package,
                    kind: PackageKind::Native,
                },
            };
        }
        if is_hashed_bundled_library(stem) {
            return not_a_package(
                "a library a wheel bundles under a hashed name (auditwheel): no package \
                 provides it, so the package that ships the wheel has to leave it out of its \
                 Requires with `%global __requires_exclude ^lib.*-[0-9a-f]{8}.*\\.so.*$`",
            );
        }
        if VENDOR_LIBRARY_PREFIXES.iter().any(|p| stem.starts_with(p)) {
            return Reported::Capability {
                capability: raw.to_string(),
                fallback: Fallback::Unbuildable(Skip::Vendor(raw.to_string())),
            };
        }
        // `libfoo` is `foo` upstream, unless that leaves too little to search for.
        let upstream = stem
            .strip_prefix("lib")
            .filter(|s| s.len() >= 3)
            .unwrap_or(stem);
        return Reported::Capability {
            capability: raw.to_string(),
            fallback: Fallback::Package {
                name: upstream.to_string(),
                kind: PackageKind::Native,
            },
        };
    }
    // Anything else with parentheses is a capability this does not know.
    if raw.contains(['(', ')']) {
        return not_a_package("an RPM capability that is not a package name");
    }

    let stripped = osc::strip_rpm_prefix(raw);
    let prefixed = stripped != raw;
    if prefixed && PYTHON_INTERPRETER_STEMS.contains(&stripped) {
        return Reported::Never(Skip::Interpreter(raw.to_string()));
    }
    // The interpreter itself: `python3`, `python313`.
    if raw
        .strip_prefix("python")
        .is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()))
    {
        return Reported::Never(Skip::Interpreter(raw.to_string()));
    }
    let name = osc::strip_devel_wrapping(stripped);
    if name.is_empty() {
        return Reported::Never(Skip::Empty);
    }
    // `--nvidia-redist`: `cuda-cublas` (or its `-devel`) is built from NVIDIA's
    // archive, never looked up on PyPI as a project of that name.
    if !prefixed && crate::redist::is_redist_package(name) {
        return Reported::Package {
            name: name.to_string(),
            kind: PackageKind::Native,
        };
    }
    if crate::pipeline::is_system_tool_alias(name) {
        return Reported::Never(Skip::SystemTool(name.to_string()));
    }
    let kind = if prefixed {
        PackageKind::Python
    } else if name != stripped {
        PackageKind::Native
    } else {
        PackageKind::Unknown
    };
    Reported::Package {
        name: name.to_string(),
        kind,
    }
}

/// The one decision for a reported name: a package to look for, or why it is
/// not something to build. `provides` asks the distro whether it provides a
/// capability; callers whose evidence is OBS saying the target has nothing
/// providing it pass a `provides` that says no.
pub fn triage(raw: &str, provides: &dyn Fn(&str) -> bool) -> Triage {
    match classify_reported(raw) {
        Reported::Never(skip) => Triage::Skip(skip),
        Reported::Capability {
            capability,
            fallback,
        } => {
            if provides(&capability) {
                return Triage::Skip(Skip::Distro(capability));
            }
            match fallback {
                Fallback::Unbuildable(skip) => Triage::Skip(skip),
                Fallback::Package { name, kind } => {
                    if provides(&name) {
                        return Triage::Skip(Skip::Distro(name));
                    }
                    Triage::Build { name, kind }
                }
            }
        }
        Reported::Package { name, kind } => {
            // A PyPI project is never a bare capability of the distro; for the
            // rest the spelling as reported (`libfoo-devel` is a real package
            // name, and `libfoo` what it is built from) is asked about too.
            if kind != PackageKind::Python {
                let raw = raw.trim();
                let undeveled = raw
                    .strip_suffix("-devel")
                    .or_else(|| raw.strip_suffix("-dev"))
                    .unwrap_or(raw);
                for candidate in [raw, undeveled, name.as_str()] {
                    if provides(candidate) {
                        return Triage::Skip(Skip::Distro(candidate.to_string()));
                    }
                }
            }
            Triage::Build { name, kind }
        }
    }
}

/// The names to queue from `raw` and, for each name that is not queued, why.
/// Every flavor of one package (`python313-fastar`, `python314-fastar`)
/// collapses to one name (compared by [`osc::fuzzy_key`], first spelling
/// kept); the names to build come back sorted.
pub fn triage_all(
    raw: &[String],
    provides: &dyn Fn(&str) -> bool,
) -> (Vec<String>, Vec<(String, Skip)>) {
    let mut seen = HashSet::new();
    let mut build = Vec::new();
    let mut skipped: Vec<(String, Skip)> = Vec::new();
    for r in raw {
        match triage(r, provides) {
            Triage::Build { name, .. } => {
                if seen.insert(osc::fuzzy_key(&name)) {
                    build.push(name);
                }
            }
            Triage::Skip(Skip::Empty) => {}
            Triage::Skip(skip) => {
                if !skipped.iter().any(|(n, s)| n == r.trim() && *s == skip) {
                    skipped.push((r.trim().to_string(), skip));
                }
            }
        }
    }
    build.sort_unstable();
    (build, skipped)
}

/// Where a package that is to be built already is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Absent,
    /// A path (linked) project ships it. Presence says nothing about the
    /// version: a bound the shipped version does not meet is not met by this.
    InLinked {
        project: String,
        pkg: String,
    },
    /// The own project has it, with the status for the repository and
    /// architecture being built (`None`: no entry for them).
    InProject {
        pkg: String,
        status: Option<BuildStatus>,
    },
}

/// Where `dep` is, matching by [`osc::fuzzy_key`] with or without the
/// `python-` prefix. The own project wins over a linked one; among several
/// spellings in it the one that is built (then in flight) is the one reported,
/// whatever order the listing came in.
pub fn availability(
    dep: &str,
    own: &HashSet<String>,
    own_statuses: &HashMap<String, BuildStatus>,
    linked: &[(String, Arc<HashSet<String>>)],
) -> Availability {
    let bare = osc::fuzzy_key(dep);
    let rpm = osc::fuzzy_key(&osc::rpm_name(dep));
    let matches = |p: &String| {
        let k = osc::fuzzy_key(p);
        k == bare || k == rpm
    };
    let rank = |p: &String| match own_statuses.get(p) {
        Some(BuildStatus::Succeeded) => 0,
        s if crate::pipeline::status_is_resolvable(s) => 1,
        Some(_) => 2,
        None => 3,
    };
    if let Some(pkg) = own
        .iter()
        .filter(|p| matches(p))
        .min_by_key(|p| (rank(p), p.len(), (*p).clone()))
    {
        return Availability::InProject {
            pkg: pkg.clone(),
            status: own_statuses.get(pkg).cloned(),
        };
    }
    for (project, listing) in linked {
        if let Some(pkg) = listing.iter().filter(|p| matches(p)).min() {
            return Availability::InLinked {
                project: project.clone(),
                pkg: pkg.clone(),
            };
        }
    }
    Availability::Absent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> bool {
        false
    }

    fn built(raw: &str) -> Option<(String, PackageKind)> {
        match triage(raw, &none) {
            Triage::Build { name, kind } => Some((name, kind)),
            Triage::Skip(_) => None,
        }
    }

    fn skipped(raw: &str) -> Skip {
        match triage(raw, &none) {
            Triage::Skip(s) => s,
            Triage::Build { name, .. } => panic!("{raw} would build {name}"),
        }
    }

    #[test]
    fn a_translated_name_keeps_the_native_kind_of_its_original() {
        let originals = vec!["libtrantor-devel".to_string(), "python313-foo".to_string()];
        let names = vec!["trantor".to_string(), "foo".to_string()];
        assert_eq!(native_among(&originals, &names), ["trantor"]);
        let ffmpeg = vec!["ffmpeg-8-devel".to_string()];
        assert_eq!(native_among(&ffmpeg, &["ffmpeg".to_string()]), ["ffmpeg"]);
        assert!(native_among(&[], &names).is_empty());
    }

    #[test]
    fn python_names_lose_their_flavor_prefix() {
        for raw in ["python313-foo", "python3-foo", "python-foo"] {
            assert_eq!(
                built(raw),
                Some(("foo".into(), PackageKind::Python)),
                "{raw}"
            );
        }
        assert_eq!(
            built("python3dist(foo-bar)"),
            Some(("foo-bar".into(), PackageKind::Python))
        );
        assert_eq!(
            built("python313dist(foo)"),
            Some(("foo".into(), PackageKind::Python))
        );
    }

    #[test]
    fn devel_packages_name_the_upstream_library() {
        assert_eq!(
            built("libtrantor-devel"),
            Some(("trantor".into(), PackageKind::Native))
        );
        assert_eq!(built("numpy"), Some(("numpy".into(), PackageKind::Unknown)));
    }

    #[test]
    fn the_interpreter_and_its_subpackages_are_never_built() {
        for raw in [
            "python3-devel",
            "python313-base",
            "python-rpm-macros",
            "python3",
            "python313",
        ] {
            assert!(matches!(skipped(raw), Skip::Interpreter(_)), "{raw}");
        }
        // A PyPI project that merely starts with a stem is still one.
        assert!(built("python313-tools-extra").is_some());
        assert!(built("python313-docker").is_some());
    }

    #[test]
    fn a_system_tool_alias_is_skipped_under_any_spelling() {
        assert!(matches!(skipped("python313-ninja"), Skip::SystemTool(_)));
        assert!(matches!(skipped("ninja"), Skip::SystemTool(_)));
    }

    #[test]
    fn a_vendor_driver_library_is_unbuildable() {
        for raw in [
            "libcuda.so.1()(64bit)",
            "libcuda.so.1",
            "libnvidia-ml.so.1()(64bit)",
        ] {
            let s = skipped(raw);
            assert!(matches!(s, Skip::Vendor(_)), "{raw}");
            assert!(s.is_unbuildable());
        }
    }

    /// python-opencv-python-headless bundles `libavcodec-f34387fd.so.62...`;
    /// dependents of it were reported missing `avcodec-f34387fd` and the like.
    #[test]
    fn a_hashed_library_bundled_in_a_wheel_is_never_a_package() {
        for raw in [
            "libavcodec-f34387fd.so.62.11.100()(64bit)",
            "libavcodec-f34387fd.so.62.11.100(LIBAVCODEC_62)(64bit)",
            "libopenblasp-r0-37b5f859.3.3.so()(64bit)",
            "libquadmath-96973f99.so.0.0.0()(64bit)",
        ] {
            let s = skipped(raw);
            assert!(matches!(s, Skip::NotAPackage { .. }), "{raw}");
            assert!(s.is_unbuildable());
        }
        // Ordinary sonames are untouched.
        assert!(built("libfoo-1.2.so.3()(64bit)").is_some());
        assert!(built("libz3.so.4.15()(64bit)").is_some());
        assert!(built("libdeadbeef-abcdefab.so.1()(64bit)").is_some());
    }

    #[test]
    fn a_soname_of_another_library_is_built_under_its_upstream_name() {
        assert_eq!(
            built("libfoo.so.2()(64bit)"),
            Some(("foo".into(), PackageKind::Native))
        );
        // Too short to search for once `lib` is gone.
        assert_eq!(
            built("libz.so.1"),
            Some(("libz".into(), PackageKind::Native))
        );
    }

    #[test]
    fn pkgconfig_and_cmake_capabilities_name_a_native_project() {
        assert_eq!(
            built("pkgconfig(libuv)"),
            Some(("libuv".into(), PackageKind::Native))
        );
        assert_eq!(
            built("cmake(Eigen3)"),
            Some(("Eigen3".into(), PackageKind::Native))
        );
    }

    #[test]
    fn files_and_rpm_features_are_not_packages() {
        for raw in [
            "/usr/bin/foo",
            "perl(Foo::Bar)",
            "rpmlib(PayloadIsZstd)",
            "weird(thing)",
        ] {
            let s = skipped(raw);
            assert!(matches!(s, Skip::NotAPackage { .. }), "{raw}");
            assert!(s.is_unbuildable());
        }
    }

    #[test]
    fn what_the_distro_provides_is_skipped_not_built() {
        let provides = |c: &str| matches!(c, "pkgconfig(zlib)" | "libpng" | "libfoo-devel");
        assert_eq!(
            triage("pkgconfig(zlib)", &provides),
            Triage::Skip(Skip::Distro("pkgconfig(zlib)".into()))
        );
        // The bare name behind a devel wrapper.
        assert_eq!(
            triage("libpng-devel", &provides),
            Triage::Skip(Skip::Distro("libpng".into()))
        );
        // The devel package's own name counts as the distro having it.
        assert_eq!(
            triage("libfoo-devel", &provides),
            Triage::Skip(Skip::Distro("libfoo-devel".into()))
        );
        // A PyPI project is not looked up as a capability.
        let always = |_: &str| true;
        assert!(matches!(
            triage("python313-six", &always),
            Triage::Build { .. }
        ));
        // Distro-provided capabilities are satisfied, not unbuildable.
        assert!(!Skip::Distro("x".into()).is_unbuildable());
    }

    #[test]
    fn triage_all_collapses_flavors_and_keeps_the_reasons() {
        let raw: Vec<String> = [
            "python313-cuda-bindings",
            "python314-cuda-bindings",
            "python3-cuda-bindings",
            "python3-devel",
            "libcuda.so.1()(64bit)",
            "  ",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (build, skipped) = triage_all(&raw, &none);
        assert_eq!(build, vec!["cuda-bindings"]);
        let names: Vec<&str> = skipped.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["python3-devel", "libcuda.so.1()(64bit)"]);
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn availability_prefers_the_built_own_spelling_and_the_own_project() {
        let own = set(&["python-foo_bar", "python-foo-bar"]);
        let statuses: HashMap<String, BuildStatus> = [
            ("python-foo_bar".to_string(), BuildStatus::Failed),
            ("python-foo-bar".to_string(), BuildStatus::Succeeded),
        ]
        .into();
        let linked = vec![(
            "openSUSE:Factory".to_string(),
            Arc::new(set(&["python-foo-bar"])),
        )];
        assert_eq!(
            availability("foo.bar", &own, &statuses, &linked),
            Availability::InProject {
                pkg: "python-foo-bar".into(),
                status: Some(BuildStatus::Succeeded)
            }
        );
    }

    #[test]
    fn availability_keeps_a_failed_own_package_visible_and_finds_linked_ones() {
        let own = set(&["python-bad"]);
        let statuses: HashMap<String, BuildStatus> =
            [("python-bad".to_string(), BuildStatus::Failed)].into();
        let linked = vec![(
            "openSUSE:Factory".to_string(),
            Arc::new(set(&["libpng16", "python-six"])),
        )];
        assert_eq!(
            availability("bad", &own, &statuses, &linked),
            Availability::InProject {
                pkg: "python-bad".into(),
                status: Some(BuildStatus::Failed)
            }
        );
        assert_eq!(
            availability("six", &own, &statuses, &linked),
            Availability::InLinked {
                project: "openSUSE:Factory".into(),
                pkg: "python-six".into()
            }
        );
        assert_eq!(
            availability("nothing", &own, &statuses, &linked),
            Availability::Absent
        );
    }
}

#[cfg(test)]
mod redist_tests {
    use super::*;
    use crate::redist::TestMode;

    fn build(raw: &str) -> Option<(String, PackageKind)> {
        match triage(raw, &|_| false) {
            Triage::Build { name, kind } => Some((name, kind)),
            Triage::Skip(_) => None,
        }
    }

    #[test]
    fn a_cuda_soname_is_a_package_to_build_only_with_the_mode_on() {
        {
            let _off = TestMode::set(false);
            // As before: the library is a vendor one, never a package.
            assert_eq!(build("libcublas.so.13()(64bit)"), None);
            assert!(matches!(
                triage("libcublas.so.13()(64bit)", &|_| false),
                Triage::Skip(Skip::Vendor(_))
            ));
        }
        let _on = TestMode::set(true);
        for soname in [
            "libcublas.so.13()(64bit)",
            "libcublasLt.so.13()(64bit)",
            "libcublas.so.13",
        ] {
            assert_eq!(
                build(soname),
                Some(("cuda-cublas".to_string(), PackageKind::Native)),
                "{soname}"
            );
        }
        assert_eq!(
            build("libcudart.so.13()(64bit)"),
            Some(("cuda-cudart".to_string(), PackageKind::Native))
        );
        assert_eq!(
            build("libcudnn.so.9()(64bit)"),
            Some(("cuda-cudnn".to_string(), PackageKind::Native))
        );
    }

    #[test]
    fn the_driver_stays_unbuildable_with_the_mode_on() {
        let _on = TestMode::set(true);
        assert!(matches!(
            triage("libcuda.so.1()(64bit)", &|_| false),
            Triage::Skip(Skip::Vendor(_))
        ));
        assert!(matches!(
            triage("libnvidia-ml.so.1()(64bit)", &|_| false),
            Triage::Skip(Skip::Vendor(_))
        ));
    }

    #[test]
    fn a_cuda_package_name_is_native_and_not_python_or_flavored() {
        let _on = TestMode::set(true);
        assert_eq!(
            build("cuda-cublas"),
            Some(("cuda-cublas".to_string(), PackageKind::Native))
        );
        assert_eq!(
            build("cuda-cublas-devel"),
            Some(("cuda-cublas".to_string(), PackageKind::Native))
        );
        assert_eq!(osc::strip_rpm_prefix("cuda-cublas"), "cuda-cublas");
        // Not a component: untouched.
        assert_eq!(build("cuda-toolkit"), Some(("cuda-toolkit".to_string(), PackageKind::Unknown)));
        // A queued wheel is available as the cuda package, by either spelling.
        let own: HashSet<String> = ["cuda-cublas".to_string()].into();
        for dep in ["nvidia-cublas", "cuda-cublas"] {
            assert!(matches!(
                availability(dep, &own, &HashMap::new(), &[]),
                Availability::InProject { .. }
            ));
        }
    }
}
