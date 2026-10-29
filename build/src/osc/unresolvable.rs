//! What a project cannot resolve: the current unresolvable packages, their
//! details and the blockers they name.

use super::*;

/// Get the unresolvable package names (and raw detail lines) OBS already
/// knows about for a package, without triggering a new build: the `_result`
/// document first, then whatever build log already sits at `log_path`. Exposed
/// so a caller that finds a package already `Unresolvable` on restart can
/// inform the fix agent immediately instead of spending an attempt
/// rediscovering it through a fresh build.
pub fn current_unresolvable(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
) -> (Vec<String>, Vec<String>) {
    // The `_result` document is what OBS says about the current revision in
    // exactly this repository and architecture; a build log may be left over
    // from an older revision, so it only runs when the document gives nothing.
    if let Some(details) = unresolvable_details(project, pkg_name, repo, arch) {
        let names = extract_unresolvable(&details);
        if !names.is_empty() {
            return (names, extract_unresolvable_detail(&details));
        }
    }
    let log_text = read_log_lossy(log_path).unwrap_or_default();
    (
        extract_unresolvable(&log_text),
        extract_unresolvable_detail(&log_text),
    )
}

/// The `<details>` text of `pkg_name`'s `unresolvable` status in this one
/// repository/architecture, or `None` when it is not unresolvable there (or
/// the status cannot be read). Other repositories' results in the same
/// document are never looked at: a package can be `unresolvable` on `i586`
/// and `succeeded` on `x86_64` at once.
pub fn unresolvable_details(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let xml = osc_api(&format!(
        "/build/{}/_result?package={}&repository={}&arch={}",
        enc(project),
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))?;
    unresolvable_details_from_xml(&xml, repo, arch, pkg_name)
}

pub(super) fn unresolvable_details_from_xml(
    xml: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
) -> Option<String> {
    let entry = obs_status::find_entry(xml, repo, arch, pkg)?;
    (BuildStatus::from(entry.code.as_str()) == BuildStatus::Unresolvable)
        .then_some(entry.details)
        .flatten()
}

/// The packages that must be built before the unresolvable `pkg_name` can
/// build, read from OBS's own `nothing provides` details for this
/// repository/architecture and mapped back to package names by
/// [`missing_package_names`]. Empty when OBS names none (not unresolvable
/// here, a version conflict, or an unreadable status).
pub fn unresolvable_blockers(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    match unresolvable_details(project, pkg_name, repo, arch) {
        Some(details) => blockers_from_details(&details),
        None => Vec::new(),
    }
}

pub(super) fn blockers_from_details(details: &str) -> Vec<String> {
    // A requirement with a version bound is the spec's own constraint failing
    // against the one version the repository has, which building the named
    // package cannot fix; only unconstrained ones are packages to build.
    let unconstrained: Vec<String> = details
        .lines()
        .flat_map(nothing_provides_segments)
        .filter(|segment| segment.split_whitespace().nth(1).is_none())
        .map(|segment| format!("nothing provides {segment}"))
        .collect();
    missing_package_names(&extract_unresolvable(&unconstrained.join("\n")))
}
