//! The synthetic submit request: a request document written from a source
//! (a package pinned to one revision) and a target, served in place of OBS's
//! own request so that the review's checks judge that action. Every other
//! read is the live one. Nothing here writes to OBS.

use std::path::{Path, PathBuf};

use crate::obs_status::ResultEntry;
use crate::osc::{DiffSpec, Listing, OpenRequest};
use crate::request_review::fetch::{ReadFailure, RequestObs};
use crate::request_review::gather::ReviewError;
use crate::request_review::report::Report;
use crate::request_review::request::{ActionSource, ActionTarget, RequestDoc, RequestState};
use crate::request_review::run::{Deps, Options, review};

/// The request as it is after the fix: the same request with its source
/// pinned to `rev`. Every other read is the live one.
pub struct Pinned<'a> {
    pub live: &'a dyn RequestObs,
    pub xml: String,
}

impl RequestObs for Pinned<'_> {
    fn request(&self, _id: &str) -> Result<String, ReadFailure> {
        Ok(self.xml.clone())
    }
    fn listing(
        &self,
        project: &str,
        pkg: &str,
        rev: Option<&str>,
    ) -> Result<Option<Listing>, ReadFailure> {
        self.live.listing(project, pkg, rev)
    }
    fn file(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: Option<&str>,
    ) -> Result<String, ReadFailure> {
        self.live.file(project, pkg, file, rev)
    }
    fn download(
        &self,
        project: &str,
        pkg: &str,
        file: &str,
        rev: &str,
        into: &Path,
    ) -> Result<PathBuf, ReadFailure> {
        self.live.download(project, pkg, file, rev, into)
    }
    fn diff(&self, spec: &DiffSpec<'_>) -> Result<String, ReadFailure> {
        self.live.diff(spec)
    }
    fn results(&self, project: &str, pkg: &str) -> Result<Option<Vec<ResultEntry>>, String> {
        self.live.results(project, pkg)
    }
    fn open_requests_to(&self, project: &str, pkg: &str) -> Result<Vec<OpenRequest>, ReadFailure> {
        self.live.open_requests_to(project, pkg)
    }
    fn current_srcmd5(&self, project: &str, pkg: &str) -> Result<String, ReadFailure> {
        self.live.current_srcmd5(project, pkg)
    }
}

/// A request document with one submit action from `source` (pinned to `rev`)
/// to `target`, in the state and by the creator of `doc`.
pub fn pinned_xml(
    doc: &RequestDoc,
    source: &ActionSource,
    target: &ActionTarget,
    rev: &str,
) -> String {
    use crate::obs_xml::escape;
    format!(
        "<request id=\"{}\" creator=\"{}\"><action type=\"submit\"><source project=\"{}\" package=\"{}\" rev=\"{}\"/><target project=\"{}\" package=\"{}\"/></action><state name=\"{}\" who=\"{}\" when=\"{}\" created=\"{}\"/></request>",
        escape(&doc.id),
        escape(&doc.creator),
        escape(&source.project),
        escape(&source.package),
        escape(rev),
        escape(&target.project),
        escape(&target.package),
        escape(&doc.state.name),
        escape(&doc.state.who),
        escape(&doc.state.when),
        escape(&doc.state.created),
    )
}

/// Review `pkg` of `branch_prj` at `rev` as a submit of that revision to
/// `origin_prj/origin_pkg`, with no request behind it. The checks are those of
/// `run::review`, run on a synthetic request with no id, creator or state (so no
/// request-level finding, and no request-date check, applies). `opts.id` is not
/// used. Nothing is written to OBS.
#[allow(clippy::too_many_arguments)]
pub fn review_branch(
    obs: &dyn RequestObs,
    branch_prj: &str,
    pkg: &str,
    rev: &str,
    origin_prj: &str,
    origin_pkg: &str,
    opts: &Options<'_>,
    deps: &Deps<'_>,
) -> Result<Report, ReviewError> {
    let source = ActionSource {
        project: branch_prj.to_string(),
        package: pkg.to_string(),
        rev: Some(rev.to_string()),
    };
    let target = ActionTarget {
        project: origin_prj.to_string(),
        package: origin_pkg.to_string(),
    };
    let doc = RequestDoc {
        id: String::new(),
        creator: String::new(),
        state: RequestState::default(),
        description: String::new(),
        actions: Vec::new(),
        reviews: Vec::new(),
    };
    let pinned = Pinned {
        live: obs,
        xml: pinned_xml(&doc, &source, &target, rev),
    };
    let options = Options {
        id: "",
        download: opts.download,
        scratch: opts.scratch,
        fail_on: opts.fail_on,
        fail_on_agent: opts.fail_on_agent,
        build: opts.build,
    };
    review(&pinned, &options, deps).map(|reviewed| reviewed.report)
}
