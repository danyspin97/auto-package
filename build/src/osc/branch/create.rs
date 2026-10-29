//! Creating branches: the branch request and its result, the paths they use,
//! the reads of a package at a revision, and the failure classification.

use super::*;
use crate::osc::branch::rebase::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;

/// Where a branched package ended up and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branched {
    pub target_project: String,
    pub target_package: String,
    /// The project the sources were copied from: where a submit request
    /// goes back to.
    pub origin_project: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchResult {
    Branched(Branched),
    /// The branch project already has the package; it is reused as it is.
    AlreadyThere,
}

/// The API request that branches `source/pkg` into `target_project`.
///
/// Unless `follow_devel`, the devel project is not followed (`ignoredevel`):
/// the package is branched from the project the caller named, which is also
/// where the resulting request goes. When following, OBS branches from the
/// package's devel project and the response says which. When `target_project`
/// does not exist OBS creates it, with the source's repositories.
pub fn branch_api_path(
    source: &str,
    pkg: &str,
    target_project: &str,
    comment: &str,
    follow_devel: bool,
) -> String {
    branch_api_path_as(source, pkg, pkg, target_project, comment, follow_devel)
}

/// [`branch_api_path`] with the branch named `target_pkg` instead of `pkg`.
pub fn branch_api_path_as(
    source: &str,
    pkg: &str,
    target_pkg: &str,
    target_project: &str,
    comment: &str,
    follow_devel: bool,
) -> String {
    branch_api_path_at(
        source,
        pkg,
        target_pkg,
        target_project,
        comment,
        follow_devel,
        None,
    )
}

/// [`branch_api_path_as`] branching the package as it was at `revision` (an
/// OBS revision number or a srcmd5; the API's `rev` parameter) instead of its
/// newest revision.
pub fn branch_api_path_at(
    source: &str,
    pkg: &str,
    target_pkg: &str,
    target_project: &str,
    comment: &str,
    follow_devel: bool,
    revision: Option<&str>,
) -> String {
    let ignore = if follow_devel { "" } else { "&ignoredevel=1" };
    let rev = revision.map_or(String::new(), |r| format!("&rev={}", enc(r)));
    format!(
        "/source/{source}/{}?cmd=branch&target_project={}&target_package={}\
         {ignore}{rev}&comment={}",
        enc(pkg),
        enc(target_project),
        enc(target_pkg),
        enc(comment)
    )
}

/// The files of `project/pkg` as they were at `revision`, or why that cannot
/// be read (most often: the package has no such revision).
pub fn package_files_at_revision(
    project: &str,
    pkg: &str,
    revision: &str,
    emit: Emit<'_>,
) -> Result<Vec<String>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg)?;
    let xml = api_get(
        &format!("/source/{project}/{}?rev={}", enc(pkg), enc(revision)),
        emit,
    )
    .map_err(|e| match e {
        Error::OscFailed { stderr, .. } => Error::NotActionable(format!(
            "{project}/{pkg} has no revision {revision} ({})",
            first_line(&stderr)
        )),
        other => other,
    })?;
    Ok(xml
        .split("<entry ")
        .skip(1)
        .filter_map(|e| crate::obs_xml::attr(&e[..e.find('>').unwrap_or(e.len())], "name"))
        .collect())
}

/// The text of `file` in `project/pkg` as it was at `revision`.
pub fn cat_file_at_revision(
    project: &str,
    pkg: &str,
    file: &str,
    revision: &str,
    emit: Emit<'_>,
) -> Option<String> {
    check_obs_name("project", project).ok()?;
    check_obs_name("package", pkg).ok()?;
    api_get(
        &format!(
            "/source/{project}/{}/{}?rev={}",
            enc(pkg),
            enc(file),
            enc(revision)
        ),
        emit,
    )
    .ok()
}

/// The devel project `<devel project="..."/>` of a package meta, if it names one.
pub fn parse_devel_project(meta_xml: &str) -> Option<String> {
    let (_, rest) = meta_xml.split_once("<devel ")?;
    let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
    crate::obs_xml::attr(tag, "project").filter(|p| !p.is_empty())
}

/// The devel project of `project/pkg`, or `None` when it has none or the meta
/// cannot be read (a hint only, so a failed read is not an error).
pub fn package_devel_project(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    check_obs_name("project", project).ok()?;
    check_obs_name("package", pkg).ok()?;
    api_get(&format!("/source/{project}/{}/_meta", enc(pkg)), emit)
        .ok()
        .and_then(|xml| parse_devel_project(&xml))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum BranchFailure {
    AlreadyThere,
    Locked,
    NotFound,
    NoPermission,
    Other,
}

/// What an OBS error body from a branch request means.
pub(super) fn classify_branch_failure(stderr: &str) -> BranchFailure {
    let lower = stderr.to_lowercase();
    // OBS' own wording only: a bare "already exists" or "locked" would also
    // match a project that exists, or a lock named in an unrelated message,
    // and a refused branch would then read as a reusable one.
    if lower.contains("double_branch_package") || lower.contains("package already exists") {
        BranchFailure::AlreadyThere
    } else if lower.contains("_locked") || lower.contains("is locked") {
        BranchFailure::Locked
    } else if is_not_found(stderr)
        || lower.contains("unknown_package")
        || lower.contains("unknown_project")
    {
        BranchFailure::NotFound
    } else if lower.contains("permission") || lower.contains("http error 403") {
        BranchFailure::NoPermission
    } else {
        BranchFailure::Other
    }
}

/// Branch `source/pkg` into `target_project`, under the same name.
pub fn branch_package(
    source: &str,
    pkg: &str,
    target_project: &str,
    follow_devel: bool,
    emit: Emit<'_>,
) -> Result<BranchResult, Error> {
    branch_package_as(source, pkg, pkg, target_project, follow_devel, emit)
}

/// Branch `source/pkg` into `target_project` as `target_pkg`. OBS names are
/// case-sensitive, so a caller whose name for the package differs from the
/// source's spelling branches it under its own.
pub fn branch_package_as(
    source: &str,
    pkg: &str,
    target_pkg: &str,
    target_project: &str,
    follow_devel: bool,
    emit: Emit<'_>,
) -> Result<BranchResult, Error> {
    branch_package_at(
        source,
        pkg,
        target_pkg,
        target_project,
        follow_devel,
        None,
        emit,
    )
}

/// [`branch_package_as`] at `revision` (a revision number or srcmd5) when
/// given, else the newest revision.
pub fn branch_package_at(
    source: &str,
    pkg: &str,
    target_pkg: &str,
    target_project: &str,
    follow_devel: bool,
    revision: Option<&str>,
    emit: Emit<'_>,
) -> Result<BranchResult, Error> {
    check_obs_name("project", source)?;
    check_obs_name("package", pkg)?;
    check_obs_name("package", target_pkg)?;
    check_obs_name("project", target_project)?;
    let path = branch_api_path_at(
        source,
        pkg,
        target_pkg,
        target_project,
        &format!("auto-package: branch for a version update of {pkg}"),
        follow_devel,
        revision,
    );
    let out = api_call(Some("POST"), &path, None, emit)?;
    if out.success() || classify_branch_failure(&out.stderr) == BranchFailure::AlreadyThere {
        // Best effort: a branch that keeps OBS's default blocking only builds
        // slower, and the build wait reports what it is blocked on.
        let _ = block_locally(target_project, emit);
    }
    if out.success() {
        return Ok(BranchResult::Branched(parse_branch_response(
            &out.stdout,
            source,
            target_pkg,
            target_project,
        )));
    }
    let why = first_line(&out.stderr).to_string();
    match classify_branch_failure(&out.stderr) {
        BranchFailure::AlreadyThere => Ok(BranchResult::AlreadyThere),
        BranchFailure::Locked => Err(Error::NotActionable(format!(
            "cannot branch {source}/{pkg}: {source} or {target_project} is locked ({why})"
        ))),
        BranchFailure::NotFound => Err(Error::NotActionable(format!(
            "cannot branch {source}/{pkg}: it does not exist ({why})"
        ))),
        BranchFailure::NoPermission => Err(Error::NotActionable(format!(
            "cannot branch {source}/{pkg} into {target_project}: permission denied ({why})"
        ))),
        BranchFailure::Other => Err(Error::OscFailed {
            cmd: format!("api -X POST {path}"),
            code: out.code,
            stderr: out.stderr,
        }),
    }
}
