//! Which architectures a package is checked on: the parse of `--arches`, the
//! architectures the origin builds per repository, and the ones a branch
//! repository must gain to match it.

use super::KNOWN_ARCHES;
use crate::osc::ProjectInfo;

/// An architecture to add to one repository of the branch project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchAddition {
    pub repository: String,
    pub arch: String,
}

/// Parse a comma separated `--arches` list. Duplicates are dropped, order
/// kept. An empty list or a name OBS does not know is an error that lists the
/// known names.
pub fn parse_arch_list(list: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in list.split(',') {
        let arch = raw.trim();
        if arch.is_empty() {
            continue;
        }
        if !KNOWN_ARCHES.contains(&arch) {
            return Err(format!(
                "unknown architecture {arch:?}; known: {}",
                KNOWN_ARCHES.join(", ")
            ));
        }
        if arch == "armv7l" {
            return Err("armv7l cannot be checked: no Tumbleweed armv7l in OBS".to_string());
        }
        if !out.iter().any(|a| a == arch) {
            out.push(arch.to_string());
        }
    }
    if out.is_empty() {
        return Err("--arches names no architecture".to_string());
    }
    Ok(out)
}

/// The architectures `repo` builds for in `project`; empty when the project
/// has no repository of that name.
pub fn repo_arches(project: &ProjectInfo, repo: &str) -> Vec<String> {
    project
        .repositories
        .iter()
        .find(|r| r.name == repo)
        .map(|r| r.arches.clone())
        .unwrap_or_default()
}

/// The architectures to add to the branch project: for every branch
/// repository the origin also has, the origin's architectures the branch
/// lacks. An architecture the origin does not build for that repository is
/// never added, so a disabled one stays disabled. `wanted`, when given (the
/// `--arches` list), narrows the additions further.
pub fn plan_additions(
    branch: &ProjectInfo,
    origin: &ProjectInfo,
    wanted: Option<&[String]>,
) -> Vec<ArchAddition> {
    let mut out = Vec::new();
    for repo in &branch.repositories {
        let origin_has = repo_arches(origin, &repo.name);
        for arch in &origin_has {
            let is_wanted = wanted.is_none_or(|w| w.iter().any(|a| a == arch));
            if is_wanted && !repo.arches.contains(arch) {
                out.push(ArchAddition {
                    repository: repo.name.clone(),
                    arch: arch.clone(),
                });
            }
        }
    }
    out
}

/// The `(repository, arch)` pairs to check once `additions` are made: each
/// branch repository's architectures, plus the added ones, narrowed to the
/// origin's architectures for that repository when the origin has it, and to
/// `wanted` when given. Sorted, without duplicates.
pub fn check_set(
    branch: &ProjectInfo,
    origin: &ProjectInfo,
    additions: &[ArchAddition],
    wanted: Option<&[String]>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for repo in &branch.repositories {
        let mut arches = repo.arches.clone();
        arches.extend(
            additions
                .iter()
                .filter(|a| a.repository == repo.name)
                .map(|a| a.arch.clone()),
        );
        let origin_has = repo_arches(origin, &repo.name);
        for arch in arches {
            let in_origin = origin_has.is_empty() || origin_has.contains(&arch);
            let is_wanted = wanted.is_none_or(|w| w.contains(&arch));
            if in_origin && is_wanted {
                out.push((repo.name.clone(), arch));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
