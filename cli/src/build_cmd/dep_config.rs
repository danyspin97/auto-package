//! The configs of the dependencies a build queues. A dependency is an ordinary
//! build of its own package: the fields that describe the root package's run
//! (an update, its branch's version, a fix, its source repository) are not
//! carried to it. Every field of `PipelineConfig` is listed in [`as_dependency`]
//! without `..`, so a field added later does not compile until it is decided
//! there whether a dependency inherits it.

use crate::*;
use ap_build::agent::PatchRequest;

/// The config a dependency of the package `config` starts from: the same run,
/// with what belongs to the root package reset.
pub(crate) fn as_dependency(config: &PipelineConfig) -> PipelineConfig {
    PipelineConfig {
        pkg_name: config.pkg_name.clone(),
        rpm_name: config.rpm_name.clone(),
        version: config.version.clone(),
        project: config.project.clone(),
        work_dir: config.work_dir.clone(),
        state_root: config.state_root.clone(),
        rpms_dir: config.rpms_dir.clone(),
        distro: config.distro.clone(),
        arch: config.arch.clone(),
        max_attempts: config.max_attempts,
        budget_usd: config.budget_usd,
        no_agent: config.no_agent,
        // The run's own flags: they apply to every package it builds.
        force: config.force,
        regen: config.regen,
        // Root only (`update` is the package the update command builds).
        update: false,
        // Root only: the checkout of an update's package is at its branch's
        // version. A dependency is at the version it is asked for.
        branch_version: None,
        // Root only: the checks of the root package's own patches.
        patch_provenance: false,
        carried_recipe: false,
        // Root only: the root's repository, tag and monorepo directories. A
        // dependency is found by its own name.
        source_repo: None,
        upstream_tag: None,
        subprojects: Vec::new(),
        // Per-run evidence shared by every package of the run.
        failed_deps: config.failed_deps.clone(),
        unbuildable_deps: config.unbuildable_deps.clone(),
        request_text: None,
        run_id: config.run_id.clone(),
        source_urls: config.source_urls.clone(),
        python_flavors: config.python_flavors.clone(),
        // Root only: the root's findings and its fix are not a dependency's.
        patch_request: None,
        patch_condition: None,
        bug_fix: None,
        unknown_build_system: false,
    }
}

/// The config of a distro package branched into (or already held by) the
/// project to be patched for another package (convention 62).
pub(crate) fn patch_config(
    config: &PipelineConfig,
    dep: &str,
    rpm_name: &str,
    version: String,
    request: &PatchRequest,
) -> PipelineConfig {
    PipelineConfig {
        pkg_name: dep.to_string(),
        rpm_name: rpm_name.to_lowercase(),
        version,
        // The branch builds green unchanged; the patch must still be made.
        force: true,
        regen: false,
        patch_request: Some(request.problem.clone()),
        patch_condition: request.condition.clone(),
        ..as_dependency(config)
    }
}

/// The config of a dependency the repository has at an older version than a
/// dependent needs, updated to `version` (from `source_repo` when it is a
/// repository, otherwise from PyPI).
pub(crate) fn outdated_config(
    config: &PipelineConfig,
    dep: &str,
    rpm_name: &str,
    version: String,
    source_repo: Option<String>,
) -> PipelineConfig {
    PipelineConfig {
        pkg_name: dep.to_string(),
        rpm_name: rpm_name.to_lowercase(),
        version,
        source_repo,
        force: true,
        regen: false,
        // A bump keeps the recipe the dependency already has.
        carried_recipe: true,
        ..as_dependency(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root of an update whose checkout is at `6.20.0`, as `build_cmd` makes it.
    fn root() -> PipelineConfig {
        PipelineConfig {
            pkg_name: "python-pyinstaller".into(),
            rpm_name: "python-pyinstaller".into(),
            version: "6.21.0".into(),
            project: "p".into(),
            work_dir: PathBuf::new(),
            state_root: PathBuf::new(),
            rpms_dir: PathBuf::new(),
            distro: "openSUSE_Tumbleweed".into(),
            arch: "x86_64".into(),
            max_attempts: 1,
            budget_usd: 1.0,
            no_agent: true,
            force: false,
            regen: false,
            update: true,
            patch_provenance: true,
            carried_recipe: true,
            branch_version: Some("6.20.0".into()),
            source_repo: Some("https://github.com/pyinstaller/pyinstaller".into()),
            upstream_tag: Some("v6.21.0".into()),
            subprojects: vec![("hooks".into(), "hooks".into())],
            failed_deps: Vec::new(),
            unknown_build_system: true,
            unbuildable_deps: Vec::new(),
            request_text: None,
            run_id: "test".into(),
            source_urls: Default::default(),
            python_flavors: Default::default(),
            patch_request: Some("the root's finding".into()),
            patch_condition: None,
            bug_fix: None,
        }
    }

    fn request() -> PatchRequest {
        PatchRequest {
            name: "libfoo".into(),
            problem: "missing symbol".into(),
            condition: None,
        }
    }

    /// The root-only fields a dependency must not carry.
    fn assert_no_root_fields(c: &PipelineConfig) {
        assert!(c.branch_version.is_none(), "branch_version");
        assert!(!c.update, "update");
        assert!(!c.patch_provenance, "patch_provenance");
        assert!(!c.unknown_build_system, "unknown_build_system");
        assert!(c.bug_fix.is_none(), "bug_fix");
        assert!(c.upstream_tag.is_none(), "upstream_tag");
        assert!(c.subprojects.is_empty(), "subprojects");
    }

    #[test]
    fn a_patched_dependency_carries_no_branch_version() {
        let c = patch_config(&root(), "libfoo", "libfoo", "1.0".into(), &request());
        assert_no_root_fields(&c);
        assert!(c.source_repo.is_none());
        assert_eq!(c.version, "1.0");
        assert_eq!(c.patch_request.as_deref(), Some("missing symbol"));
    }

    #[test]
    fn an_outdated_dependency_carries_no_branch_version() {
        let repo = Some("https://github.com/a/b".to_string());
        let c = outdated_config(&root(), "libfoo", "libfoo", "2.0".into(), repo.clone());
        assert_no_root_fields(&c);
        assert_eq!(c.source_repo, repo);
        assert!(c.upstream_tag.is_none() && c.subprojects.is_empty());
        assert!(c.patch_request.is_none());
        assert!(c.carried_recipe, "a bumped dependency carries its recipe");
    }

    #[test]
    fn a_dependency_carries_no_branch_version() {
        let c = as_dependency(&root());
        assert_no_root_fields(&c);
        assert!(c.source_repo.is_none() && c.patch_request.is_none());
        assert!(!c.carried_recipe, "a dependency is not a carried recipe");
        assert_eq!(c.run_id, "test");
    }

    #[test]
    fn deps_rs_builds_no_dependency_config_by_copying_the_root() {
        // Every config in deps.rs goes through this module's helpers, which
        // reset the root's fields; a struct update from the root would not.
        let src = include_str!("deps.rs");
        assert!(
            !src.contains("..config.clone()"),
            "deps.rs copies the root's config"
        );
        assert!(src.contains("patch_config(") && src.contains("outdated_config("));
    }
}
