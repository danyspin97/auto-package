use super::*;

/// How many names one search request carries.
pub(super) const SEARCH_CHUNK: usize = 20;

/// What the resolver asks of OBS. A trait so the logic runs against fixtures.
pub trait Obs: Send + Sync {
    /// The meta XML of a project, `None` when it does not exist.
    fn project_meta(&self, project: &str) -> Result<Option<String>, String>;
    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String>;
    /// The published binaries named any of `names`, in any project.
    fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String>;
}

/// [`Obs`] over `osc`.
pub struct LiveObs {
    pub reporter: Arc<dyn Reporter + Send + Sync>,
}

impl Obs for LiveObs {
    fn project_meta(&self, project: &str) -> Result<Option<String>, String> {
        osc::get_project_meta(project, Emit::run(self.reporter.as_ref())).map_err(|e| e.to_string())
    }

    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String> {
        osc::set_project_meta(project, xml, Emit::run(self.reporter.as_ref()))
            .map_err(|e| e.to_string())
    }

    fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        osc::search_published_binaries(names, Emit::quiet()).map_err(|e| e.to_string())
    }
}

/// The meta OBS gives a branch project it creates: each repository of the
/// source, building for the same architectures, with a path to that
/// repository of the source (so the branch builds against what the source
/// publishes).
pub fn synthetic_branch_meta(branch: &str, source: &str, source_info: &ProjectInfo) -> String {
    let esc = crate::obs_xml::escape;
    let mut xml = format!("<project name=\"{}\">\n", esc(branch));
    for repo in &source_info.repositories {
        xml.push_str(&format!("  <repository name=\"{}\">\n", esc(&repo.name)));
        xml.push_str(&format!(
            "    <path project=\"{}\" repository=\"{}\"/>\n",
            esc(source),
            esc(&repo.name)
        ));
        for arch in &repo.arches {
            xml.push_str(&format!("    <arch>{}</arch>\n", esc(arch)));
        }
        xml.push_str("  </repository>\n");
    }
    xml.push_str("</project>\n");
    xml
}

/// [`Obs`] for a dry run: a branch project that does not exist yet reads as
/// the meta branching would give it, and nothing can be written.
pub struct DryRunObs<O: Obs> {
    pub inner: O,
    pub branch_project: String,
    pub branch_meta: String,
}

impl<O: Obs> Obs for DryRunObs<O> {
    fn project_meta(&self, project: &str) -> Result<Option<String>, String> {
        match self.inner.project_meta(project)? {
            None if project == self.branch_project => Ok(Some(self.branch_meta.clone())),
            other => Ok(other),
        }
    }

    fn set_project_meta(&self, project: &str, _xml: &str) -> Result<(), String> {
        Err(format!("dry run: not editing {project}"))
    }

    fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        self.inner.published_binaries(names)
    }
}
