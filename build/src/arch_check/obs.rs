//! The OBS calls the arch check makes, behind a trait so that its logic runs
//! against fixtures. [`LiveArchObs`] makes them with `osc`.

use std::path::Path;

use crate::obs_status::ResultEntry;
use crate::osc::{
    fetch_buildlog, get_package_results, get_package_results_if_present, get_project_meta,
    set_project_meta,
};
use crate::proc::Emit;

/// What the arch check asks of OBS about the projects it reads and writes.
pub trait ArchObs {
    /// The meta XML of a project, `None` when the project does not exist.
    fn project_meta(&self, project: &str) -> Result<Option<String>, String>;
    /// Replace the meta XML of a project.
    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String>;
    /// Every result of `pkg` in `project`: all repositories, architectures and
    /// multibuild flavors.
    fn results(&self, project: &str, pkg: &str) -> Result<Vec<ResultEntry>, String>;
    /// [`ArchObs::results`], but `None` when OBS has no such project or package
    /// (a 404): a baseline may lack the package, which is no result, not an error.
    fn results_if_present(
        &self,
        project: &str,
        pkg: &str,
    ) -> Result<Option<Vec<ResultEntry>>, String>;
    /// The build log of `pkg` for `(repo, arch)` in `project`, saved at `path`.
    fn buildlog(
        &self,
        project: &str,
        pkg: &str,
        repo: &str,
        arch: &str,
        path: &Path,
    ) -> Result<(), String>;
}

/// [`ArchObs`] over `osc`.
pub struct LiveArchObs<'a> {
    pub emit: Emit<'a>,
}

impl ArchObs for LiveArchObs<'_> {
    fn project_meta(&self, project: &str) -> Result<Option<String>, String> {
        get_project_meta(project, self.emit).map_err(|e| e.to_string())
    }

    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String> {
        set_project_meta(project, xml, self.emit).map_err(|e| e.to_string())
    }

    fn results(&self, project: &str, pkg: &str) -> Result<Vec<ResultEntry>, String> {
        get_package_results(project, pkg)
    }

    fn results_if_present(
        &self,
        project: &str,
        pkg: &str,
    ) -> Result<Option<Vec<ResultEntry>>, String> {
        get_package_results_if_present(project, pkg)
    }

    fn buildlog(
        &self,
        project: &str,
        pkg: &str,
        repo: &str,
        arch: &str,
        path: &Path,
    ) -> Result<(), String> {
        fetch_buildlog(project, pkg, repo, arch, path, self.emit).map_err(|e| e.to_string())
    }
}
