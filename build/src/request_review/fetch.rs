//! What the review reads from OBS, behind a trait so the checks run against
//! fixtures. The trait has only read methods; [`LiveRequestObs`] makes them
//! with `osc`.

use std::path::{Path, PathBuf};

use crate::obs_status::ResultEntry;
use crate::osc::{self, DiffSpec, Listing, OpenRequest};
use crate::proc::Emit;

/// Why a read gave no answer: what failed, and the HTTP status when OBS gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFailure {
    pub what: String,
    pub http: Option<u16>,
}

impl ReadFailure {
    pub fn new(what: impl Into<String>) -> Self {
        ReadFailure {
            what: what.into(),
            http: None,
        }
    }

    fn from_osc(e: osc::Error) -> Self {
        let http = match &e {
            osc::Error::OscFailed { stderr, .. } => osc::http_status(stderr),
            _ => None,
        };
        ReadFailure {
            what: e.to_string(),
            http,
        }
    }
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.what)
    }
}

/// Every read of a review. Nothing here writes to OBS.
pub trait RequestObs {
    /// The request document.
    fn request(&self, id: &str) -> Result<String, ReadFailure>;
    /// The files of `project/pkg` at `rev` (current without one). `None`: OBS
    /// has no such package or revision.
    fn listing(
        &self,
        project: &str,
        pkg: &str,
        rev: Option<&str>,
    ) -> Result<Option<Listing>, ReadFailure>;
    /// The text of a file at `rev` (current without one).
    fn file(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: Option<&str>,
    ) -> Result<String, ReadFailure>;
    /// A file at `rev` written under `into`; where it is.
    fn download(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: &str,
        into: &Path,
    ) -> Result<PathBuf, ReadFailure>;
    /// The unified diff of the source against the target.
    fn diff(&self, spec: &DiffSpec<'_>) -> Result<String, ReadFailure>;
    /// The build results of `project/pkg`; `None` when OBS has none.
    fn results(&self, project: &str, pkg: &str) -> Result<Option<Vec<ResultEntry>>, String>;
    /// The open submit requests to `project/pkg`.
    fn open_requests_to(&self, project: &str, pkg: &str) -> Result<Vec<OpenRequest>, ReadFailure>;
    /// The current `srcmd5` of `project/pkg`, as a listing at a revision gives
    /// it (expanded).
    fn current_srcmd5(&self, project: &str, pkg: &str) -> Result<String, ReadFailure>;
}

/// [`RequestObs`] over `osc`.
pub struct LiveRequestObs<'a> {
    pub emit: Emit<'a>,
}

impl RequestObs for LiveRequestObs<'_> {
    fn request(&self, id: &str) -> Result<String, ReadFailure> {
        osc::request_document(id, self.emit).map_err(ReadFailure::from_osc)
    }

    fn listing(
        &self,
        project: &str,
        pkg: &str,
        rev: Option<&str>,
    ) -> Result<Option<Listing>, ReadFailure> {
        osc::package_entries_at_revision(project, pkg, rev, self.emit)
            .map_err(ReadFailure::from_osc)
    }

    fn file(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: Option<&str>,
    ) -> Result<String, ReadFailure> {
        osc::cat_file_expanded(project, pkg, file, rev, self.emit).map_err(ReadFailure::from_osc)
    }

    fn download(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: &str,
        into: &Path,
    ) -> Result<PathBuf, ReadFailure> {
        osc::fetch_file_at_revision(project, pkg, file, rev, into, self.emit)
            .map_err(ReadFailure::from_osc)
    }

    fn diff(&self, spec: &DiffSpec<'_>) -> Result<String, ReadFailure> {
        osc::source_diff(spec, self.emit).map_err(ReadFailure::from_osc)
    }

    fn results(&self, project: &str, pkg: &str) -> Result<Option<Vec<ResultEntry>>, String> {
        osc::get_package_results_if_present(project, pkg)
    }

    fn open_requests_to(&self, project: &str, pkg: &str) -> Result<Vec<OpenRequest>, ReadFailure> {
        osc::open_requests_to(project, pkg, self.emit).map_err(ReadFailure::from_osc)
    }

    fn current_srcmd5(&self, project: &str, pkg: &str) -> Result<String, ReadFailure> {
        // The expanded listing's revision, the one a listing at the request's
        // revision gives, so the two compare.
        self.listing(project, pkg, None)?
            .and_then(|l| l.srcmd5)
            .ok_or_else(|| ReadFailure::new(format!("{project}/{pkg} has no revision")))
    }
}
