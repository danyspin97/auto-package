//! Branching, project-meta build paths, binary search and submit requests:
//! the OBS operations behind `update`.
//!
//! Everything that builds an argument list, a query or a document is a pure
//! function with its own tests; the thin wrappers that run `osc` around them
//! need a live OBS and are not covered.

use std::time::Duration;

use super::{
    Error, OSC_QUERY_TIMEOUT, anywhere, check_obs_name, during, enc, first_line, is_not_found,
    is_valid_obs_name, parse_project_repo_paths,
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
    crate::obs_xml::attr(tag, "error").filter(|e| !e.is_empty())
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
    let listing_path = format!("/source/{project}/{}", enc(pkg));
    // A link whose base is already the origin's current revision has nothing
    // to rebase onto, and OBS answers the request with a 404 then.
    if !linkinfo_is_behind(&api_get(&listing_path, emit)?) {
        return check_link(project, pkg, emit);
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
    check_link(project, pkg, emit)
}

/// Whether a package listing's `<linkinfo>` shows the link based on a revision
/// of its origin other than the current one. Unknown counts as behind, so the
/// rebase is still asked for.
fn linkinfo_is_behind(xml: &str) -> bool {
    let Some(rest) = xml.find("<linkinfo").map(|i| &xml[i..]) else {
        return true;
    };
    let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
    match (
        crate::obs_xml::attr(tag, "srcmd5"),
        crate::obs_xml::attr(tag, "baserev"),
    ) {
        (Some(current), Some(base)) => current != base,
        _ => true,
    }
}

/// The state of the link after a rebase (or when none was needed): a conflict
/// leaves sources that cannot be built or submitted.
fn check_link(project: &str, pkg: &str, emit: Emit<'_>) -> Result<Rebased, Error> {
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

/// `xml` with `block="local"` on every `<repository>` that sets no `block`,
/// or `None` when there is nothing to change.
///
/// OBS blocks a repository by default until every package of the projects on
/// its build paths has finished building. A branch has the paths of the
/// project it came from, so while that project (Base:System) is mid-rebuild
/// the branched package waits with it, for as long as the rebuild takes.
/// `local` blocks only on the branch project's own packages.
pub fn with_local_blocking(xml: &str) -> Option<String> {
    let mut out = xml.to_string();
    let mut changed = false;
    // Back to front, so earlier offsets stay valid.
    for (start, _) in repository_spans(xml).into_iter().rev() {
        let open_end = out[start..].find('>').map_or(out.len(), |i| start + i);
        if crate::obs_xml::attr(&out[start..open_end], "block").is_some() {
            continue;
        }
        let at = if out[..open_end].ends_with('/') {
            open_end - 1
        } else {
            open_end
        };
        let trimmed = out[..at].trim_end().len();
        out.insert_str(trimmed, " block=\"local\"");
        changed = true;
    }
    changed.then_some(out)
}

/// Make the repositories of `project` block on its own packages only (see
/// [`with_local_blocking`]).
fn block_locally(project: &str, emit: Emit<'_>) -> Result<(), Error> {
    let Some(xml) = get_project_meta(project, emit)? else {
        return Ok(());
    };
    match with_local_blocking(&xml) {
        Some(edited) => set_project_meta(project, &edited, emit),
        None => Ok(()),
    }
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
        .and_then(|tag| crate::obs_xml::attr(tag, "kind"))
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
            let name = crate::obs_xml::attr(open, "name")?;
            let arches = block
                .split("<arch>")
                .skip(1)
                .filter_map(|c| c.split("</arch>").next())
                .map(|a| crate::obs_xml::unescape(a.trim()))
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
        .tempfile_in(proc::private_dir()?)?;
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
            crate::obs_xml::escape(&self.project),
            crate::obs_xml::escape(&self.path_repository)
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
                crate::obs_xml::attr(&xml[s..open_end], "name").as_deref()
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
                name: crate::obs_xml::attr(tag, "name")?,
                project: crate::obs_xml::attr(tag, "project")?,
                repository: crate::obs_xml::attr(tag, "repository")?,
                arch: crate::obs_xml::attr(tag, "arch")?,
                package: crate::obs_xml::attr(tag, "package"),
                version: crate::obs_xml::attr(tag, "version"),
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

#[cfg(test)]
mod tests;
