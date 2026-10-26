//! Branching, project-meta build paths, binary search and submit requests:
//! the OBS operations behind `update`.
//!
//! Everything that builds an argument list, a query or a document is a pure
//! function with its own tests; the thin wrappers that run `osc` around them
//! need a live OBS and are not covered.

use std::time::Duration;

use super::{
    Error, OSC_QUERY_TIMEOUT, anywhere, check_obs_name, during, enc, extract_xml_attr, first_line,
    is_not_found, is_valid_obs_name, parse_project_repo_paths, xml_escape,
};
use crate::proc::{self, Captured, Emit};

/// Run `osc api` with an optional HTTP method and body.
fn api_call(
    method: Option<&str>,
    path: &str,
    data: Option<&str>,
    emit: Emit<'_>,
) -> Result<Captured, Error> {
    let mut args: Vec<String> = vec!["api".into()];
    if let Some(m) = method {
        args.extend(["-X".into(), m.into()]);
    }
    if let Some(d) = data {
        args.extend(["-d".into(), d.into()]);
    }
    args.push(path.into());
    proc::capture_with_timeout("osc", &args, &anywhere(), emit, Duration::from_secs(120))
        .map_err(during(format!("api {path}")))
}

/// `GET path` through `osc api`, with a failure reported as an error.
fn api_get(path: &str, emit: Emit<'_>) -> Result<String, Error> {
    let out = api_call(None, path, None, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("api {path}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(out.stdout)
}

/// The text of `<data name="NAME">value</data>` in an OBS status document.
fn xml_data_value(xml: &str, name: &str) -> Option<String> {
    let open = format!("<data name=\"{name}\">");
    let (_, rest) = xml.split_once(&open)?;
    let (value, _) = rest.split_once("</data>")?;
    Some(value.trim().to_string())
}

/// The OBS user `osc` is configured for. Newer `osc` has `whoami`; older
/// versions only have `whois`, which prints `name: "Full Name" <email>`.
pub fn current_user(emit: Emit<'_>) -> Result<String, Error> {
    let mut last = None;
    for cmd in ["whoami", "whois"] {
        let out = proc::capture_with_timeout("osc", &[cmd], &anywhere(), emit, OSC_QUERY_TIMEOUT)
            .map_err(during(cmd))?;
        if !out.success() {
            last = Some(Error::OscFailed {
                cmd: cmd.into(),
                code: out.code,
                stderr: out.stderr,
            });
            continue;
        }
        return parse_whoami(&out.stdout).ok_or_else(|| {
            Error::NotActionable(format!(
                "could not read the OBS user from `osc {cmd}` (got {:?}); pass --branch-project",
                first_line(&out.stdout)
            ))
        });
    }
    Err(last.expect("two commands were tried"))
}

/// The user name in `osc whoami` or `osc whois` output: the bare name, a
/// `Username: name` line, or `name: "Full Name" <email>`.
fn parse_whoami(stdout: &str) -> Option<String> {
    let line = first_line(stdout);
    let name = line.strip_prefix("Username:").unwrap_or(line).trim();
    let name = name.split_whitespace().next()?.trim_end_matches(':');
    is_valid_obs_name(name).then(|| name.to_string())
}

/// The project `osc branch` puts a branch of `source` in when no target is
/// named.
pub fn default_branch_project(user: &str, source: &str) -> String {
    format!("home:{user}:branches:{source}")
}

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
    let ignore = if follow_devel { "" } else { "&ignoredevel=1" };
    format!(
        "/source/{source}/{}?cmd=branch&target_project={}&target_package={}\
         {ignore}&comment={}",
        enc(pkg),
        enc(target_project),
        enc(pkg),
        enc(comment)
    )
}

/// The devel project `<devel project="..."/>` of a package meta, if it names one.
pub fn parse_devel_project(meta_xml: &str) -> Option<String> {
    let (_, rest) = meta_xml.split_once("<devel ")?;
    let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
    extract_xml_attr(tag, "project").filter(|p| !p.is_empty())
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
enum BranchFailure {
    AlreadyThere,
    Locked,
    NotFound,
    NoPermission,
    Other,
}

/// What an OBS error body from a branch request means.
fn classify_branch_failure(stderr: &str) -> BranchFailure {
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

/// Branch `source/pkg` into `target_project`.
pub fn branch_package(
    source: &str,
    pkg: &str,
    target_project: &str,
    follow_devel: bool,
    emit: Emit<'_>,
) -> Result<BranchResult, Error> {
    check_obs_name("project", source)?;
    check_obs_name("package", pkg)?;
    check_obs_name("project", target_project)?;
    let path = branch_api_path(
        source,
        pkg,
        target_project,
        &format!("auto-package: branch for a version update of {pkg}"),
        follow_devel,
    );
    let out = api_call(Some("POST"), &path, None, emit)?;
    if out.success() {
        return Ok(BranchResult::Branched(parse_branch_response(
            &out.stdout,
            source,
            pkg,
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

/// The API request that rebases a branched package onto the current sources
/// of its origin (what `osc rebase` does).
pub fn rebase_api_path(project: &str, pkg: &str) -> String {
    format!("/source/{project}/{}?cmd=rebase", enc(pkg))
}

/// What [`rebase_package`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebased {
    /// The package is not a link (a plain copy), so there is nothing to rebase.
    NotALink,
    /// The link was rebased and still expands cleanly.
    Clean,
    /// The link no longer expands: the rebase left a conflict (or the link is
    /// broken). The reason is OBS's own.
    Conflict(String),
}

/// The `error` OBS puts on a package listing's `<linkinfo>` for a link that
/// is broken or in conflict with its origin.
fn linkinfo_error(xml: &str) -> Option<String> {
    let rest = &xml[xml.find("<linkinfo")?..];
    let tag = &rest[..rest.find('>')?];
    extract_xml_attr(tag, "error").filter(|e| !e.is_empty())
}

/// Rebase the branched `project/pkg` onto its origin's current sources, so a
/// branch reused from an earlier run does not build, and later submit, on
/// sources the origin has since moved past. A package that is not a link is
/// left alone, and the state after the rebase is checked: a conflict leaves
/// sources that cannot be built or submitted.
pub fn rebase_package(project: &str, pkg: &str, emit: Emit<'_>) -> Result<Rebased, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg)?;
    let link = api_call(
        None,
        &format!("/source/{project}/{}/_link", enc(pkg)),
        None,
        emit,
    )?;
    if !link.success() && is_not_found(&link.stderr) {
        return Ok(Rebased::NotALink);
    }
    let path = rebase_api_path(project, pkg);
    let out = api_call(Some("POST"), &path, None, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("api -X POST {path}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    let listing = api_get(&format!("/source/{project}/{}", enc(pkg)), emit)?;
    if let Some(e) = linkinfo_error(&listing) {
        return Ok(Rebased::Conflict(e));
    }
    // A conflicting link only fails when expanded.
    let expanded = api_call(
        None,
        &format!("/source/{project}/{}?expand=1", enc(pkg)),
        None,
        emit,
    )?;
    if !expanded.success() {
        return Ok(Rebased::Conflict(first_line(&expanded.stderr).to_string()));
    }
    Ok(Rebased::Clean)
}

/// Delete `project/pkg`. Only for a package this run has just created, such
/// as a branch that came out under a name nothing will ever build.
pub fn delete_package(project: &str, pkg: &str, emit: Emit<'_>) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg)?;
    let path = format!("/source/{project}/{}", enc(pkg));
    let out = api_call(Some("DELETE"), &path, None, emit)?;
    if out.success() {
        return Ok(());
    }
    Err(Error::OscFailed {
        cmd: format!("api -X DELETE {path}"),
        code: out.code,
        stderr: out.stderr,
    })
}

/// Read a branch response, falling back to what was asked for when a field
/// is missing.
fn parse_branch_response(xml: &str, source: &str, pkg: &str, target_project: &str) -> Branched {
    Branched {
        target_project: xml_data_value(xml, "targetproject")
            .unwrap_or_else(|| target_project.to_string()),
        target_package: xml_data_value(xml, "targetpackage").unwrap_or_else(|| pkg.to_string()),
        origin_project: xml_data_value(xml, "sourceproject").unwrap_or_else(|| source.to_string()),
    }
}

/// One `<repository>` of a project meta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDef {
    pub name: String,
    /// `(project, repository)` build paths, in order.
    pub paths: Vec<(String, String)>,
    pub arches: Vec<String>,
}

/// What `update` needs to know about a project meta.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectInfo {
    /// The `kind` attribute: absent for a standard project, `maintenance`,
    /// `maintenance_incident` or `maintenance_release` otherwise.
    pub kind: Option<String>,
    pub locked: bool,
    pub repositories: Vec<RepoDef>,
}

/// Byte ranges `(start, end)` of every `<repository>` element in `xml`, end
/// exclusive and past the closing tag.
fn repository_spans(xml: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(rel) = xml[from..].find("<repository") {
        let start = from + rel;
        let after = start + "<repository".len();
        // `<repositoryfoo` is some other element: only whitespace after the
        // name makes it this one.
        if !xml[after..].starts_with(|c: char| c.is_whitespace()) {
            from = after;
            continue;
        }
        let Some(open_end) = xml[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let end = if xml[..open_end].ends_with("/>") {
            open_end
        } else {
            match xml[open_end..].find("</repository>") {
                Some(i) => open_end + i + "</repository>".len(),
                None => break,
            }
        };
        spans.push((start, end));
        from = end;
    }
    spans
}

/// Parsing half of the project-meta read, split out so it is testable
/// without shelling out to `osc`.
pub fn parse_project_info(xml: &str) -> ProjectInfo {
    let kind = xml
        .find("<project ")
        .and_then(|i| xml[i..].find('>').map(|e| &xml[i..i + e]))
        .and_then(|tag| extract_xml_attr(tag, "kind"))
        .filter(|k| !k.is_empty() && k != "standard");
    let locked = xml
        .find("<lock>")
        .and_then(|i| xml[i..].find("</lock>").map(|e| &xml[i..i + e]))
        .is_some_and(|block| block.contains("<enable"));
    let repositories = repository_spans(xml)
        .into_iter()
        .filter_map(|(s, e)| {
            let block = &xml[s..e];
            let open = &block[..block.find('>')? + 1];
            let name = extract_xml_attr(open, "name")?;
            let arches = block
                .split("<arch>")
                .skip(1)
                .filter_map(|c| c.split("</arch>").next())
                .map(|a| crate::osc::xml_unescape(a.trim()))
                .collect();
            Some(RepoDef {
                name,
                paths: parse_project_repo_paths(block),
                arches,
            })
        })
        .collect();
    ProjectInfo {
        kind,
        locked,
        repositories,
    }
}

/// The meta XML of `project`, or `None` when the project does not exist.
pub fn get_project_meta(project: &str, emit: Emit<'_>) -> Result<Option<String>, Error> {
    check_obs_name("project", project)?;
    let cmd = format!("meta prj {project}");
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if out.success() {
        return Ok(Some(out.stdout));
    }
    if is_not_found(&out.stderr) || out.stderr.contains("unknown_project") {
        return Ok(None);
    }
    Err(Error::OscFailed {
        cmd,
        code: out.code,
        stderr: out.stderr,
    })
}

/// Replace the meta of `project` with `xml`.
pub fn set_project_meta(project: &str, xml: &str, emit: Emit<'_>) -> Result<(), Error> {
    check_obs_name("project", project)?;
    super::invalidate_repo_paths(project);
    let mut file = tempfile::Builder::new()
        .prefix("ap-prj-meta-")
        .suffix(".xml")
        .tempfile_in(proc::private_dir())?;
    std::io::Write::write_all(&mut file, xml.as_bytes())?;
    let path = file.path().to_string_lossy().into_owned();
    let cmd = format!("meta prj {project} -F");
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project, "-F", &path],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// A build path to add to one repository of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAddition {
    /// The repository of the project being edited.
    pub repository: String,
    pub project: String,
    /// The repository of `project` the path points at.
    pub path_repository: String,
}

impl PathAddition {
    fn element(&self) -> String {
        format!(
            "<path project=\"{}\" repository=\"{}\"/>",
            xml_escape(&self.project),
            xml_escape(&self.path_repository)
        )
    }
}

/// The whitespace leading the line `pos` is on.
fn line_indent(xml: &str, pos: usize) -> &str {
    let line_start = xml[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line = &xml[line_start..pos];
    &line[..line.len() - line.trim_start().len()]
}

/// Add build paths to a project meta, after the paths a repository already
/// has (a path listed earlier wins when two provide the same package, so an
/// added one only ever supplies what the existing ones lack).
///
/// A path the repository already lists is left alone, so running this again
/// changes nothing. Returns the new XML and the additions actually made. A
/// repository the meta does not have is an error.
pub fn add_paths_to_meta(
    xml: &str,
    additions: &[PathAddition],
) -> Result<(String, Vec<PathAddition>), String> {
    let mut xml = xml.to_string();
    let mut made = Vec::new();
    for add in additions {
        let (start, end) = repository_spans(&xml)
            .into_iter()
            .find(|&(s, _)| {
                let open_end = xml[s..].find('>').map_or(xml.len(), |i| s + i + 1);
                extract_xml_attr(&xml[s..open_end], "name").as_deref()
                    == Some(add.repository.as_str())
            })
            .ok_or_else(|| format!("the project meta has no repository {}", add.repository))?;
        let block = &xml[start..end];
        if parse_project_repo_paths(block)
            .contains(&(add.project.clone(), add.path_repository.clone()))
        {
            continue;
        }
        let element = add.element();
        let open_end = start + block.find('>').unwrap_or(0) + 1;
        let repo_indent = line_indent(&xml, start).to_string();

        if xml[..open_end].ends_with("/>") {
            // `<repository name="x"/>` has no body to insert into.
            let open = xml[start..open_end].trim_end_matches("/>").trim_end();
            let rewritten =
                format!("{open}>\n{repo_indent}  {element}\n{repo_indent}</repository>");
            xml.replace_range(start..end, &rewritten);
        } else if let Some(rel) = block.rfind("<path ") {
            // Right after the last existing path.
            let at = start + rel;
            let close = xml[at..].find('>').map_or(end, |i| at + i + 1);
            let indent = line_indent(&xml, at).to_string();
            let rest = if indent.is_empty() && !xml[..at].ends_with('\n') {
                // Inline XML: no line to copy the indentation from.
                element
            } else {
                format!("\n{indent}{element}")
            };
            xml.insert_str(close, &rest);
        } else {
            // Paths come before arches in the schema; with neither, before
            // the closing tag.
            let anchor = block
                .find("<arch>")
                .map_or(end - "</repository>".len(), |rel| start + rel);
            let own_line = xml[..anchor]
                .rfind('\n')
                .is_some_and(|nl| xml[nl + 1..anchor].trim().is_empty());
            if own_line {
                let line_start = xml[..anchor].rfind('\n').unwrap_or(0) + 1;
                let indent = if xml[anchor..].starts_with("</repository>") {
                    format!("{repo_indent}  ")
                } else {
                    line_indent(&xml, anchor).to_string()
                };
                xml.insert_str(line_start, &format!("{indent}{element}\n"));
            } else {
                xml.insert_str(anchor, &element);
            }
        }
        made.push(add.clone());
    }
    Ok((xml, made))
}

/// A binary OBS has published, as `/search/published/binary/id` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedBinary {
    pub name: String,
    pub project: String,
    pub repository: String,
    pub arch: String,
    /// The source package it was built from.
    pub package: Option<String>,
    pub version: Option<String>,
}

/// Whether `name` can be put in a published-binary search at all. A name with
/// a quote or a backslash cannot be written into the query, so it is never
/// searched, and an answer about it is unknown rather than "nothing publishes
/// it".
pub fn is_searchable_binary_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\'', '"', '\\'])
}

/// The search request for published binaries named any of `names`, across
/// every project. Names [`is_searchable_binary_name`] rejects are left out.
pub fn published_binary_search_path(names: &[&str]) -> String {
    let terms: Vec<String> = names
        .iter()
        .filter(|n| is_searchable_binary_name(n))
        .map(|n| format!("@name='{n}'"))
        .collect();
    format!(
        "/search/published/binary/id?match={}",
        enc(&terms.join(" or "))
    )
}

/// Parsing half of [`search_published_binaries`].
pub fn parse_published_binaries(xml: &str) -> Vec<PublishedBinary> {
    xml.split("<binary ")
        .skip(1)
        .filter_map(|chunk| {
            let tag = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
            Some(PublishedBinary {
                name: extract_xml_attr(tag, "name")?,
                project: extract_xml_attr(tag, "project")?,
                repository: extract_xml_attr(tag, "repository")?,
                arch: extract_xml_attr(tag, "arch")?,
                package: extract_xml_attr(tag, "package"),
                version: extract_xml_attr(tag, "version"),
            })
        })
        .collect()
}

/// Every published binary named any of `names`, in any project.
pub fn search_published_binaries(
    names: &[&str],
    emit: Emit<'_>,
) -> Result<Vec<PublishedBinary>, Error> {
    if names.iter().all(|n| !is_searchable_binary_name(n)) {
        return Ok(Vec::new());
    }
    let xml = api_get(&published_binary_search_path(names), emit)?;
    Ok(parse_published_binaries(&xml))
}

/// A submit request to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRequest {
    pub source_project: String,
    pub source_package: String,
    pub target_project: String,
    pub target_package: String,
    pub description: String,
}

/// The request document `POST /request?cmd=create` takes.
pub fn submit_request_xml(sr: &SubmitRequest) -> String {
    format!(
        "<request>\n  <action type=\"submit\">\n    \
         <source project=\"{}\" package=\"{}\"/>\n    \
         <target project=\"{}\" package=\"{}\"/>\n  </action>\n  \
         <description>{}</description>\n</request>\n",
        xml_escape(&sr.source_project),
        xml_escape(&sr.source_package),
        xml_escape(&sr.target_project),
        xml_escape(&sr.target_package),
        xml_escape(&sr.description),
    )
}

/// The id in a created request's response.
pub fn parse_request_id(xml: &str) -> Option<String> {
    let (_, rest) = xml.split_once("<request ")?;
    let (tag, _) = rest.split_once('>')?;
    extract_xml_attr(tag, "id")
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

/// Create a submit request and return its id.
pub fn create_submit_request(sr: &SubmitRequest, emit: Emit<'_>) -> Result<String, Error> {
    check_obs_name("project", &sr.source_project)?;
    check_obs_name("package", &sr.source_package)?;
    check_obs_name("project", &sr.target_project)?;
    check_obs_name("package", &sr.target_package)?;
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
        let Some(id) = extract_xml_attr(head, "id") else {
            continue;
        };
        let state = req
            .split("<state ")
            .nth(1)
            .and_then(|s| extract_xml_attr(&s[..s.find('>').unwrap_or(s.len())], "name"))
            .unwrap_or_default();
        for action in req.split("<action ").skip(1) {
            let action = action.split("</action>").next().unwrap_or(action);
            let head = &action[..action.find('>').unwrap_or(action.len())];
            if extract_xml_attr(head, "type").as_deref() != Some("submit") {
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
                extract_xml_attr(source, "project"),
                extract_xml_attr(source, "package"),
                extract_xml_attr(target, "project"),
                extract_xml_attr(target, "package"),
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

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = r#"<project name="home:u:branches:Base:System">
  <title>Branch</title>
  <description/>
  <person userid="u" role="maintainer"/>
  <repository name="openSUSE_Tumbleweed">
    <path project="Base:System" repository="openSUSE_Tumbleweed"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="15.6">
    <path project="Base:System" repository="15.6"/>
    <path project="openSUSE:Factory" repository="snapshot"/>
    <arch>x86_64</arch>
  </repository>
</project>
"#;

    fn add(repo: &str, project: &str, path_repo: &str) -> PathAddition {
        PathAddition {
            repository: repo.into(),
            project: project.into(),
            path_repository: path_repo.into(),
        }
    }

    #[test]
    fn a_broken_link_shows_in_the_linkinfo_error() {
        let ok = r#"<directory name="p"><linkinfo project="o" package="p" srcmd5="a" xsrcmd5="b"/></directory>"#;
        assert_eq!(linkinfo_error(ok), None);
        let bad = r#"<directory name="p"><linkinfo project="o" package="p" error="conflict in file p.spec"/></directory>"#;
        assert_eq!(
            linkinfo_error(bad).as_deref(),
            Some("conflict in file p.spec")
        );
        assert_eq!(linkinfo_error("<directory/>"), None);
    }

    #[test]
    fn whoami_reads_a_bare_name_or_a_labelled_one() {
        assert_eq!(parse_whoami("danyspin97\n").as_deref(), Some("danyspin97"));
        assert_eq!(parse_whoami("Username: dany\n").as_deref(), Some("dany"));
        assert_eq!(
            parse_whoami("dspinella: \"Danilo Spinella\" <d@example.org>\n").as_deref(),
            Some("dspinella"),
            "the osc whois form"
        );
        assert_eq!(parse_whoami(""), None);
        assert_eq!(parse_whoami("-rf\n"), None);
    }

    #[test]
    fn default_branch_project_is_what_osc_branch_uses() {
        assert_eq!(
            default_branch_project("dany", "Base:System"),
            "home:dany:branches:Base:System"
        );
    }

    #[test]
    fn branch_path_names_source_package_and_target() {
        assert_eq!(
            branch_api_path(
                "Base:System",
                "libsigc++",
                "home:u:branches:Base:System",
                "a b",
                false
            ),
            "/source/Base:System/libsigc%2B%2B?cmd=branch\
             &target_project=home%3Au%3Abranches%3ABase%3ASystem\
             &target_package=libsigc%2B%2B&ignoredevel=1&comment=a%20b"
        );
    }

    #[test]
    fn rebase_path_names_the_branched_package() {
        assert_eq!(
            rebase_api_path("home:u:branches:S", "libsigc++"),
            "/source/home:u:branches:S/libsigc%2B%2B?cmd=rebase"
        );
    }

    #[test]
    fn following_the_devel_project_drops_ignoredevel() {
        let path = branch_api_path("S", "p", "T", "c", true);
        assert!(!path.contains("ignoredevel"), "{path}");
        assert!(branch_api_path("S", "p", "T", "c", false).contains("&ignoredevel=1&"));
    }

    #[test]
    fn the_devel_project_comes_from_the_package_meta() {
        let meta = r#"<package name="foo" project="Base:System">
  <title/>
  <devel project="devel:languages:python" package="foo"/>
</package>"#;
        assert_eq!(
            parse_devel_project(meta).as_deref(),
            Some("devel:languages:python")
        );
        assert_eq!(
            parse_devel_project("<package name=\"foo\"><title/></package>"),
            None
        );
    }

    #[test]
    fn branch_response_gives_the_origin_and_falls_back_to_the_request() {
        let xml = r#"<status code="ok"><summary>Ok</summary>
  <data name="targetproject">home:u:branches:X</data>
  <data name="targetpackage">foo</data>
  <data name="sourceproject">devel:languages:c_c++</data>
  <data name="sourcepackage">foo</data>
</status>"#;
        assert_eq!(
            parse_branch_response(xml, "X", "foo", "home:u:branches:X"),
            Branched {
                target_project: "home:u:branches:X".into(),
                target_package: "foo".into(),
                origin_project: "devel:languages:c_c++".into(),
            }
        );
        assert_eq!(
            parse_branch_response("<status code=\"ok\"/>", "X", "foo", "T").origin_project,
            "X"
        );
    }

    #[test]
    fn branch_failures_are_told_apart() {
        use BranchFailure::*;
        let c = classify_branch_failure;
        assert_eq!(
            c("<status code=\"double_branch_package\">x</status>"),
            AlreadyThere
        );
        assert_eq!(c("branch target package already exists: a/b"), AlreadyThere);
        assert_eq!(c("project 'Base:System' is locked"), Locked);
        assert_eq!(c("<status code=\"project_locked\">x</status>"), Locked);
        // Neither word alone says what happened.
        assert_eq!(c("create_project: project already exists"), Other);
        assert_eq!(c("unlocked mirror lookup failed"), Other);
        assert_eq!(c("HTTP Error 404: Not Found"), NotFound);
        assert_eq!(c("unknown_package"), NotFound);
        assert_eq!(c("create_project_no_permission"), NoPermission);
        assert_eq!(c("HTTP Error 500"), Other);
    }

    #[test]
    fn project_info_reads_kind_lock_and_repositories() {
        let info = parse_project_info(META);
        assert_eq!(info.kind, None);
        assert!(!info.locked);
        assert_eq!(info.repositories.len(), 2);
        assert_eq!(info.repositories[0].name, "openSUSE_Tumbleweed");
        assert_eq!(info.repositories[0].arches, ["x86_64", "aarch64"]);
        assert_eq!(
            info.repositories[1].paths,
            [
                ("Base:System".to_string(), "15.6".to_string()),
                ("openSUSE:Factory".to_string(), "snapshot".to_string())
            ]
        );

        let maint = "<project name=\"M\" kind=\"maintenance\"><lock><enable/></lock></project>";
        let info = parse_project_info(maint);
        assert_eq!(info.kind.as_deref(), Some("maintenance"));
        assert!(info.locked);
        assert!(
            !parse_project_info("<project name=\"M\"><lock><disable/></lock></project>").locked
        );
    }

    #[test]
    fn project_info_unescapes_names_once() {
        let xml = "<project name=\"M\"><repository name=\"a&amp;lt;&#x41;&#66;&bogus;&quot;\">\
                   <arch>x&amp;86</arch></repository></project>";
        let info = parse_project_info(xml);
        assert_eq!(info.repositories[0].name, "a&lt;AB&bogus;\"");
        assert_eq!(info.repositories[0].arches, ["x&86"]);
    }

    #[test]
    fn a_path_goes_after_the_last_existing_one() {
        let (xml, made) = add_paths_to_meta(META, &[add("15.6", "devel:tools", "15.6")]).unwrap();
        assert_eq!(made.len(), 1);
        assert!(xml.contains(
            "    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>\n    \
             <path project=\"devel:tools\" repository=\"15.6\"/>\n    <arch>x86_64</arch>"
        ));
        // The other repository is untouched.
        let info = parse_project_info(&xml);
        assert_eq!(info.repositories[0].paths.len(), 1);
        assert_eq!(info.repositories[1].paths.len(), 3);
    }

    #[test]
    fn adding_a_path_twice_changes_nothing() {
        let adds = [
            add("15.6", "devel:tools", "15.6"),
            add("openSUSE_Tumbleweed", "devel:tools", "openSUSE_Tumbleweed"),
        ];
        let (once, made) = add_paths_to_meta(META, &adds).unwrap();
        assert_eq!(made.len(), 2);
        let (twice, made) = add_paths_to_meta(&once, &adds).unwrap();
        assert!(made.is_empty());
        assert_eq!(once, twice);
        // A path the meta already had is not added either.
        let (same, made) = add_paths_to_meta(META, &[add("15.6", "Base:System", "15.6")]).unwrap();
        assert!(made.is_empty());
        assert_eq!(same, META);
    }

    #[test]
    fn several_repositories_are_edited_independently() {
        let adds = [
            add("openSUSE_Tumbleweed", "devel:a", "openSUSE_Tumbleweed"),
            add("15.6", "devel:a", "15.6"),
            add("15.6", "devel:b", "15.6"),
        ];
        let (xml, made) = add_paths_to_meta(META, &adds).unwrap();
        assert_eq!(made, adds);
        let info = parse_project_info(&xml);
        let tw: Vec<_> = info.repositories[0]
            .paths
            .iter()
            .map(|p| p.0.as_str())
            .collect();
        assert_eq!(tw, ["Base:System", "devel:a"]);
        let leap: Vec<_> = info.repositories[1]
            .paths
            .iter()
            .map(|p| p.0.as_str())
            .collect();
        assert_eq!(
            leap,
            ["Base:System", "openSUSE:Factory", "devel:a", "devel:b"]
        );
        // The arches survive.
        assert_eq!(info.repositories[0].arches, ["x86_64", "aarch64"]);
    }

    #[test]
    fn a_repository_without_paths_gets_them_before_its_arches() {
        let meta = "<project name=\"p\">\n  <repository name=\"r\">\n    \
                    <arch>x86_64</arch>\n  </repository>\n</project>\n";
        let (xml, _) = add_paths_to_meta(meta, &[add("r", "devel:a", "x")]).unwrap();
        assert_eq!(
            xml,
            "<project name=\"p\">\n  <repository name=\"r\">\n    \
             <path project=\"devel:a\" repository=\"x\"/>\n    \
             <arch>x86_64</arch>\n  </repository>\n</project>\n"
        );
    }

    #[test]
    fn a_repository_with_neither_paths_nor_arches_gets_a_path_in_its_body() {
        let meta = "<project name=\"p\">\n  <repository name=\"r\">\n  </repository>\n</project>\n";
        let (xml, _) = add_paths_to_meta(meta, &[add("r", "devel:a", "x")]).unwrap();
        assert!(xml.contains("<repository name=\"r\">\n"), "{xml}");
        assert_eq!(parse_project_info(&xml).repositories[0].paths.len(), 1);
    }

    #[test]
    fn a_self_closing_repository_is_opened_up() {
        let meta = "<project name=\"p\">\n  <repository name=\"r\"/>\n</project>\n";
        let (xml, _) = add_paths_to_meta(meta, &[add("r", "devel:a", "x")]).unwrap();
        assert_eq!(
            xml,
            "<project name=\"p\">\n  <repository name=\"r\">\n    \
             <path project=\"devel:a\" repository=\"x\"/>\n  </repository>\n</project>\n"
        );
    }

    #[test]
    fn a_repository_the_meta_lacks_is_an_error() {
        let err = add_paths_to_meta(META, &[add("nope", "a", "b")]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn repository_names_that_only_share_a_prefix_are_not_confused() {
        let meta = "<project name=\"p\">\n  <repository name=\"15\">\n    \
                    <arch>x86_64</arch>\n  </repository>\n  <repository name=\"15.6\">\n    \
                    <arch>x86_64</arch>\n  </repository>\n</project>\n";
        let (xml, _) = add_paths_to_meta(meta, &[add("15.6", "devel:a", "x")]).unwrap();
        let info = parse_project_info(&xml);
        assert!(info.repositories[0].paths.is_empty());
        assert_eq!(info.repositories[1].paths.len(), 1);
    }

    #[test]
    fn binary_search_ors_the_names_and_skips_quoted_ones() {
        assert_eq!(
            published_binary_search_path(&["foo", "foo-devel", "a'b"]),
            "/search/published/binary/id?match=%40name%3D%27foo%27%20or%20%40name%3D%27foo-devel%27"
        );
    }

    #[test]
    fn names_with_quotes_or_backslashes_are_not_searchable() {
        assert!(is_searchable_binary_name("libfoo-devel"));
        assert!(is_searchable_binary_name("gtk+-3.0"));
        for bad in ["", "a'b", "a\"b", "a\\b"] {
            assert!(!is_searchable_binary_name(bad), "{bad:?}");
        }
        assert!(
            search_published_binaries(&["a'b"], Emit::quiet())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn published_binaries_are_read_from_the_search_result() {
        let xml = r#"<collection matches="2">
  <binary name="libfoo-devel" project="devel:languages:c_c++" package="foo" repository="openSUSE_Tumbleweed" version="1.2" release="3.1" arch="x86_64" filename="libfoo-devel-1.2-3.1.x86_64.rpm" type="rpm"/>
  <binary name="libfoo-devel" project="openSUSE:Factory" repository="snapshot" arch="x86_64"/>
  <binary name="broken" project="p"/>
</collection>"#;
        let bins = parse_published_binaries(xml);
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].package.as_deref(), Some("foo"));
        assert_eq!(bins[0].version.as_deref(), Some("1.2"));
        assert_eq!(bins[1].package, None);
        assert_eq!(bins[1].repository, "snapshot");
    }

    fn sr() -> SubmitRequest {
        SubmitRequest {
            source_project: "home:u:branches:Base:System".into(),
            source_package: "foo".into(),
            target_project: "Base:System".into(),
            target_package: "foo".into(),
            description: "Update to 2 (was 1) & <more>".into(),
        }
    }

    #[test]
    fn submit_request_xml_names_both_ends_and_escapes_the_description() {
        assert_eq!(
            submit_request_xml(&sr()),
            "<request>\n  <action type=\"submit\">\n    \
             <source project=\"home:u:branches:Base:System\" package=\"foo\"/>\n    \
             <target project=\"Base:System\" package=\"foo\"/>\n  </action>\n  \
             <description>Update to 2 (was 1) &amp; &lt;more&gt;</description>\n</request>\n"
        );
    }

    #[test]
    fn request_id_is_read_from_the_response() {
        assert_eq!(
            parse_request_id("<request id=\"1234567\" creator=\"u\">\n</request>").as_deref(),
            Some("1234567")
        );
        assert_eq!(parse_request_id("<status code=\"err\"/>"), None);
        assert_eq!(parse_request_id("<request id=\"x\">"), None);
        // Cut off before the closing `>`, and multi-byte text around.
        assert_eq!(parse_request_id("<request id=\"12\""), None);
        assert_eq!(xml_data_value("<data name=\"a\">é", "a"), None);
        assert_eq!(
            xml_data_value("é<data name=\"a\"> v </data>é", "a").as_deref(),
            Some("v")
        );
    }

    #[test]
    fn open_requests_query_asks_for_open_submits_from_the_source() {
        assert_eq!(
            open_requests_path("home:u:branches:X", "foo"),
            "/request?view=collection&roles=source&types=submit&states=new,review\
             &project=home%3Au%3Abranches%3AX&package=foo"
        );
    }

    const REQUESTS: &str = r#"<collection matches="3">
  <request id="101" creator="u">
    <action type="submit">
      <source project="home:u:branches:Base:System" package="foo" rev="3"/>
      <target project="Base:System" package="foo"/>
    </action>
    <state name="review" who="u" when="2026-10-01T00:00:00"/>
    <description>Update foo</description>
  </request>
  <request id="102" creator="u">
    <action type="submit">
      <source project="home:u:branches:Base:System" package="foo"/>
      <target project="openSUSE:Factory" package="foo"/>
    </action>
    <action type="delete"><target project="x" package="y"/></action>
    <state name="new"/>
  </request>
  <request id="103"><action type="submit"><source project="a"/></action></request>
</collection>"#;

    #[test]
    fn open_requests_are_parsed_per_submit_action() {
        let reqs = parse_open_requests(REQUESTS);
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].id, "101");
        assert_eq!(reqs[0].state, "review");
        assert_eq!(reqs[0].target_project, "Base:System");
        assert_eq!(reqs[1].id, "102");
        assert_eq!(reqs[1].target_project, "openSUSE:Factory");
    }

    #[test]
    fn a_request_to_another_target_is_not_a_duplicate() {
        let reqs = parse_open_requests(REQUESTS);
        let dup: Vec<_> = reqs
            .iter()
            .filter(|r| r.target_project == "Base:System" && r.source_package == "foo")
            .collect();
        assert_eq!(dup.len(), 1);
        assert_eq!(dup[0].id, "101");
        assert!(parse_open_requests("<collection matches=\"0\"/>").is_empty());
    }
}
