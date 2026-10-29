//! Submit requests: creating one, the srcmd5 checks of the source it names, and
//! the listing of open requests.

use super::*;
use crate::osc::error::*;
use crate::osc::names::*;

/// A submit request to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRequest {
    pub source_project: String,
    pub source_package: String,
    pub target_project: String,
    pub target_package: String,
    pub description: String,
    /// The `srcmd5` of the source package the request was prepared and checked
    /// for. The request is pinned to it, and creating it is refused when the
    /// source has moved on.
    pub source_rev: Option<String>,
}

/// The request document `POST /request?cmd=create` takes.
pub fn submit_request_xml(sr: &SubmitRequest) -> String {
    format!(
        "<request>\n  <action type=\"submit\">\n    \
         <source project=\"{}\" package=\"{}\"{}/>\n    \
         <target project=\"{}\" package=\"{}\"/>\n  </action>\n  \
         <description>{}</description>\n</request>\n",
        crate::obs_xml::escape(&sr.source_project),
        crate::obs_xml::escape(&sr.source_package),
        sr.source_rev
            .as_deref()
            .map(|r| format!(" rev=\"{}\"", crate::obs_xml::escape(r)))
            .unwrap_or_default(),
        crate::obs_xml::escape(&sr.target_project),
        crate::obs_xml::escape(&sr.target_package),
        crate::obs_xml::escape(&sr.description),
    )
}

/// The id in a created request's response.
pub fn parse_request_id(xml: &str) -> Option<String> {
    let (_, rest) = xml.split_once("<request ")?;
    let (tag, _) = rest.split_once('>')?;
    crate::obs_xml::attr(tag, "id")
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

/// The `srcmd5` in a package listing's `<directory>` tag.
pub fn parse_directory_srcmd5(xml: &str) -> Option<String> {
    let rest = &xml[xml.find("<directory")?..];
    let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
    crate::obs_xml::attr(tag, "srcmd5").filter(|r| !r.is_empty())
}

/// The current `srcmd5` of `project/package`.
pub fn source_srcmd5(project: &str, package: &str, emit: Emit<'_>) -> Result<String, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", package)?;
    let xml = api_get(&format!("/source/{project}/{}", enc(package)), emit)?;
    parse_directory_srcmd5(&xml).ok_or_else(|| {
        Error::NotActionable(format!("OBS returned no revision for {project}/{package}"))
    })
}

/// Refuse a request whose source package is no longer at the revision it was
/// prepared for. `current` reads the revision now.
pub fn verify_source_rev(
    sr: &SubmitRequest,
    current: impl FnOnce() -> Result<String, Error>,
) -> Result<(), Error> {
    let Some(expected) = &sr.source_rev else {
        return Ok(());
    };
    let now = current()?;
    if &now != expected {
        return Err(Error::NotActionable(format!(
            "{}/{} changed since the request was prepared (revision {expected}, now {now}); \
             nothing was submitted, check it again",
            sr.source_project, sr.source_package
        )));
    }
    Ok(())
}

/// Create a submit request and return its id.
pub fn create_submit_request(sr: &SubmitRequest, emit: Emit<'_>) -> Result<String, Error> {
    check_obs_name("project", &sr.source_project)?;
    check_obs_name("package", &sr.source_package)?;
    check_obs_name("project", &sr.target_project)?;
    check_obs_name("package", &sr.target_package)?;
    verify_source_rev(sr, || {
        source_srcmd5(&sr.source_project, &sr.source_package, emit)
    })?;
    let out = api_call(
        Some("POST"),
        "/request?cmd=create",
        Some(&submit_request_xml(sr)),
        emit,
    )?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: "api -X POST /request?cmd=create".into(),
            code: out.code,
            stderr: out.stderr,
        });
    }
    parse_request_id(&out.stdout).ok_or_else(|| {
        Error::NotActionable(format!(
            "OBS accepted the request but returned no request id: {}",
            first_line(&out.stdout)
        ))
    })
}

/// A submit request found open on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    pub id: String,
    pub source_project: String,
    pub source_package: String,
    pub target_project: String,
    pub target_package: String,
    pub state: String,
}

/// The query for the open (`new` or in `review`) submit requests whose source
/// is `source_project/source_package`.
pub fn open_requests_path(source_project: &str, source_package: &str) -> String {
    format!(
        "/request?view=collection&roles=source&types=submit&states=new,review&project={}&package={}",
        enc(source_project),
        enc(source_package)
    )
}

/// Parsing half of [`open_submit_requests`]: the submit actions of every
/// request in a collection.
pub fn parse_open_requests(xml: &str) -> Vec<OpenRequest> {
    let mut out = Vec::new();
    for req in xml.split("<request ").skip(1) {
        let req = req.split("</request>").next().unwrap_or(req);
        let head = &req[..req.find('>').unwrap_or(req.len())];
        let Some(id) = crate::obs_xml::attr(head, "id") else {
            continue;
        };
        let state = req
            .split("<state ")
            .nth(1)
            .and_then(|s| crate::obs_xml::attr(&s[..s.find('>').unwrap_or(s.len())], "name"))
            .unwrap_or_default();
        for action in req.split("<action ").skip(1) {
            let action = action.split("</action>").next().unwrap_or(action);
            let head = &action[..action.find('>').unwrap_or(action.len())];
            if crate::obs_xml::attr(head, "type").as_deref() != Some("submit") {
                continue;
            }
            let element = |name: &str| {
                action
                    .split(name)
                    .nth(1)
                    .map(|t| &t[..t.find('>').unwrap_or(t.len())])
            };
            let (Some(source), Some(target)) = (element("<source "), element("<target ")) else {
                continue;
            };
            let (Some(sp), Some(spkg), Some(tp), Some(tpkg)) = (
                crate::obs_xml::attr(source, "project"),
                crate::obs_xml::attr(source, "package"),
                crate::obs_xml::attr(target, "project"),
                crate::obs_xml::attr(target, "package"),
            ) else {
                continue;
            };
            out.push(OpenRequest {
                id: id.clone(),
                source_project: sp,
                source_package: spkg,
                target_project: tp,
                target_package: tpkg,
                state: state.clone(),
            });
        }
    }
    out
}

/// The open submit requests from `source_project/source_package` to
/// `target_project`: what makes a new one a duplicate.
pub fn open_submit_requests(
    source_project: &str,
    source_package: &str,
    target_project: &str,
    emit: Emit<'_>,
) -> Result<Vec<OpenRequest>, Error> {
    let xml = api_get(&open_requests_path(source_project, source_package), emit)?;
    Ok(parse_open_requests(&xml)
        .into_iter()
        .filter(|r| {
            r.source_project == source_project
                && r.source_package == source_package
                && r.target_project == target_project
        })
        .collect())
}
