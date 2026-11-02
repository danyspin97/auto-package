//! The fake OBS the check tests run against, and the builders of its metas and
//! results. Every read and write is recorded in `events`, so the tests can
//! assert what was asked of OBS and in what order.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::obs_status::ResultEntry;

pub(super) const ORIGIN: &str = "openSUSE:Factory";
pub(super) const BRANCH: &str = "home:u:branches:openSUSE:Factory";

/// OBS in memory. `events` records each call in order; a meta read made under
/// the project-meta lock is marked `(locked)`.
#[derive(Default)]
pub(super) struct FakeObs {
    pub(super) metas: RefCell<HashMap<String, String>>,
    pub(super) results: RefCell<HashMap<String, Vec<ResultEntry>>>,
    /// Projects OBS answers 404 for: `results_if_present` is `None` for them.
    pub(super) absent: RefCell<HashSet<String>>,
    /// Log text per `(pkg, repo, arch)`; a missing one cannot be fetched.
    pub(super) logs: RefCell<HashMap<(String, String, String), String>>,
    pub(super) events: RefCell<Vec<String>>,
}

impl FakeObs {
    pub(super) fn log(&self, event: String) {
        self.events.borrow_mut().push(event);
    }

    pub(super) fn writes(&self) -> Vec<String> {
        self.events
            .borrow()
            .iter()
            .filter(|e| e.starts_with("write"))
            .cloned()
            .collect()
    }
}

impl ArchObs for FakeObs {
    fn project_meta(&self, project: &str) -> Result<Option<String>, String> {
        let locked = if crate::osc::project_meta_lock_is_free() {
            ""
        } else {
            "(locked)"
        };
        self.log(format!("read{locked}:{project}"));
        Ok(self.metas.borrow().get(project).cloned())
    }

    fn set_project_meta(&self, project: &str, xml: &str) -> Result<(), String> {
        self.log(format!("write:{project}"));
        self.metas
            .borrow_mut()
            .insert(project.to_string(), xml.to_string());
        Ok(())
    }

    fn results(&self, project: &str, pkg: &str) -> Result<Vec<ResultEntry>, String> {
        self.log(format!("results:{project}:{pkg}"));
        self.results
            .borrow()
            .get(project)
            .cloned()
            .ok_or_else(|| format!("{project}: no results"))
    }

    fn results_if_present(
        &self,
        project: &str,
        pkg: &str,
    ) -> Result<Option<Vec<ResultEntry>>, String> {
        if self.absent.borrow().contains(project) {
            self.log(format!("results:{project}:{pkg}"));
            return Ok(None);
        }
        self.results(project, pkg).map(Some)
    }

    fn buildlog(
        &self,
        _project: &str,
        pkg: &str,
        repo: &str,
        arch: &str,
        path: &Path,
    ) -> Result<(), String> {
        self.log(format!("log:{pkg}:{repo}:{arch}"));
        let key = (pkg.to_string(), repo.to_string(), arch.to_string());
        let text = self.logs.borrow().get(&key).cloned().ok_or("no log")?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

pub(super) fn meta(project: &str, repos: &[(&str, &[&str])]) -> String {
    let mut xml = format!("<project name=\"{project}\">\n");
    for (name, arches) in repos {
        xml.push_str(&format!("  <repository name=\"{name}\">\n"));
        for a in *arches {
            xml.push_str(&format!("    <arch>{a}</arch>\n"));
        }
        xml.push_str("  </repository>\n");
    }
    xml.push_str("</project>\n");
    xml
}

pub(super) fn entry(pkg: &str, repo: &str, arch: &str, code: &str) -> ResultEntry {
    ResultEntry {
        repo: repo.into(),
        arch: arch.into(),
        dirty: false,
        pkg: pkg.into(),
        code: code.into(),
        details: None,
    }
}

/// Origin builds x86_64 and s390x; the branch builds x86_64 only.
pub(super) fn fixture() -> FakeObs {
    let fake = FakeObs::default();
    fake.metas.borrow_mut().insert(
        ORIGIN.into(),
        meta(ORIGIN, &[("standard", &["x86_64", "s390x"])]),
    );
    fake.metas
        .borrow_mut()
        .insert(BRANCH.into(), meta(BRANCH, &[("standard", &["x86_64"])]));
    fake
}

/// The branch also builds s390x, and each result is set by the caller.
pub(super) fn with_branch_s390x(fake: &FakeObs) {
    fake.metas.borrow_mut().insert(
        BRANCH.into(),
        meta(BRANCH, &[("standard", &["x86_64", "s390x"])]),
    );
}

pub(super) fn set_results(fake: &FakeObs, project: &str, entries: Vec<ResultEntry>) {
    fake.results.borrow_mut().insert(project.into(), entries);
}

/// OBS answers 404 for `project`: it has no such project or package.
pub(super) fn set_absent(fake: &FakeObs, project: &str) {
    fake.absent.borrow_mut().insert(project.into());
}

pub(super) fn request<'a>(
    log_dir: &'a Path,
    say: &'a dyn Fn(String),
    add_arches: bool,
    read_only: bool,
) -> CheckRequest<'a> {
    CheckRequest {
        branch: BRANCH,
        origin: ORIGIN,
        pkg: "p",
        primary: ("standard", "x86_64"),
        wanted: None,
        add_arches,
        read_only,
        bound: Duration::ZERO,
        log_dir,
        say,
    }
}
