//! A [`RequestObs`] over documents held in memory, recording each call.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::fetch::{ReadFailure, RequestObs};
use crate::obs_status::ResultEntry;
use crate::osc::{DiffSpec, Listing, OpenRequest};

#[derive(Default)]
pub struct FakeRequestObs {
    pub request: HashMap<String, String>,
    /// `(project, package, rev)` to the listing; a missing key is a 404.
    pub listings: HashMap<(String, String, Option<String>), Listing>,
    /// `(project, package, file, rev)` to the text.
    pub files: HashMap<(String, String, String, Option<String>), String>,
    pub diff: Option<String>,
    pub results: HashMap<(String, String), Vec<ResultEntry>>,
    pub open: Vec<OpenRequest>,
    pub srcmd5: Option<String>,
    /// Methods that fail, by name, with the failure.
    pub failing: HashMap<&'static str, ReadFailure>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeRequestObs {
    fn record(&self, call: &str) -> Result<(), ReadFailure> {
        self.calls.borrow_mut().push(call.to_string());
        let name = call.split_whitespace().next().unwrap_or(call);
        match self.failing.get(name) {
            Some(f) => Err(f.clone()),
            None => Ok(()),
        }
    }

    pub fn called(&self, name: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.split_whitespace().next() == Some(name))
            .count()
    }
}

impl RequestObs for FakeRequestObs {
    fn request(&self, id: &str) -> Result<String, ReadFailure> {
        self.record("request")?;
        self.request.get(id).cloned().ok_or(ReadFailure {
            what: format!("request {id}"),
            http: Some(404),
        })
    }

    fn listing(
        &self,
        project: &str,
        pkg: &str,
        rev: Option<&str>,
    ) -> Result<Option<Listing>, ReadFailure> {
        self.record(&format!("listing {project}/{pkg}"))?;
        let key = (project.into(), pkg.into(), rev.map(String::from));
        Ok(self.listings.get(&key).cloned())
    }

    fn file(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: Option<&str>,
    ) -> Result<String, ReadFailure> {
        self.record(&format!("file {project}/{pkg}/{file}"))?;
        let key = (
            project.into(),
            pkg.into(),
            file.into(),
            rev.map(String::from),
        );
        self.files
            .get(&key)
            .cloned()
            .ok_or_else(|| ReadFailure::new(format!("no {file}")))
    }

    fn download(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        _rev: &str,
        into: &Path,
    ) -> Result<PathBuf, ReadFailure> {
        self.record(&format!("download {project}/{pkg}/{file}"))?;
        let path = into.join(file);
        std::fs::write(&path, file).map_err(|e| ReadFailure::new(e.to_string()))?;
        Ok(path)
    }

    fn diff(&self, _spec: &DiffSpec<'_>) -> Result<String, ReadFailure> {
        self.record("diff")?;
        self.diff.clone().ok_or_else(|| ReadFailure::new("no diff"))
    }

    fn results(&self, project: &str, pkg: &str) -> Result<Option<Vec<ResultEntry>>, String> {
        self.record("results").map_err(|e| e.what)?;
        Ok(self.results.get(&(project.into(), pkg.into())).cloned())
    }

    fn open_requests_to(
        &self,
        _project: &str,
        _pkg: &str,
    ) -> Result<Vec<OpenRequest>, ReadFailure> {
        self.record("open_requests_to")?;
        Ok(self.open.clone())
    }

    fn current_srcmd5(&self, _project: &str, _pkg: &str) -> Result<String, ReadFailure> {
        self.record("current_srcmd5")?;
        self.srcmd5
            .clone()
            .ok_or_else(|| ReadFailure::new("no srcmd5"))
    }
}
