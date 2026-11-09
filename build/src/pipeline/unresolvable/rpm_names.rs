//! Whether an RPM on disk satisfies a name a wait record waits on.
//!
//! A record keeps the source names of its dependencies (`foo`), while the RPM
//! of a Python package carries a flavour (`python313-foo-2026.7-1.1.noarch.rpm`,
//! or `python-foo`, or `python3-foo`). A local build keeps its RPMs in the
//! `local` directory under the RPM directory. `rpm_satisfies_dep` matches the
//! exact flavoured name OBS reports and reads one directory only, so it cannot
//! answer for these.

use super::*;
use crate::pipeline::rpm_deps::{parse_rpm_dep_constraint, rpm_version_satisfies};

/// The name and version of a binary RPM file, `<name>-<version>-<release>.<arch>.rpm`.
/// The version and the release hold no `-`, so the two last `-` separate them
/// from the name, which may hold `-` of its own (`python313-foo-bar`).
pub(super) fn rpm_file_name_version(file: &str) -> Option<(&str, &str)> {
    let stem = file.strip_suffix(".rpm")?;
    let (rest, arch) = stem.rsplit_once('.')?;
    if arch.is_empty() || arch.contains('-') || matches!(arch, "src" | "nosrc") {
        return None;
    }
    let (name_version, release) = rest.rsplit_once('-')?;
    let (name, version) = name_version.rsplit_once('-')?;
    if name.is_empty() || release.is_empty() || !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some((name, version))
}

/// The bare comparison key of a package name: any constraint and any flavour
/// prefix removed, separators and case ignored (`python313-Foo_bar` and
/// `foo-bar` are the same key).
fn bare_key(name: &str) -> String {
    let (base, _) = parse_rpm_dep_constraint(name);
    osc::fuzzy_key(osc::strip_rpm_prefix(base))
}

/// Whether the RPM name `rpm` and the dependency name `dep` name one package,
/// whatever flavour each carries. `foo` does not name `foo-bar`.
pub(super) fn same_package(rpm: &str, dep: &str) -> bool {
    let key = bare_key(rpm);
    !key.is_empty() && key == bare_key(dep)
}

/// Whether an RPM in one of `dirs` is the package `dep` names, at a version
/// every bound in `outdated` for it accepts. OBS reported the installed version
/// too old for each bound, so an RPM is taken only when it is at least the
/// bound. A bound with no version can not be checked, so no RPM meets it.
pub(super) fn wait_dep_satisfied(
    dirs: &[&Path],
    dep: &str,
    outdated: &[(String, Option<String>)],
) -> bool {
    let bounds: Vec<Option<&str>> = outdated
        .iter()
        .filter(|(name, _)| same_package(name, dep))
        .map(|(_, bound)| bound.as_deref())
        .collect();
    dirs.iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .any(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            let Some((name, version)) = rpm_file_name_version(&file) else {
                return false;
            };
            same_package(name, dep)
                && bounds.iter().all(
                    |bound| matches!(bound, Some(req) if rpm_version_satisfies(version, ">=", req)),
                )
        })
}
