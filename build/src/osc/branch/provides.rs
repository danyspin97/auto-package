//! What a published binary provides: the RPMs a search names, and the
//! `Provides:` each RPM declares (the `fileinfo_ext` view of a repository
//! file). A capability such as `pkgconfig(glib-2.0) >= 2.70` is provided by
//! the RPM whose `Provides:` says so; the name of that RPM is not needed.

use super::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;

/// One RPM a binary search found, with the repository it is published in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRpm {
    pub name: String,
    pub project: String,
    pub repository: String,
    pub arch: String,
    pub package: Option<String>,
}

/// The answer to a binary search: its RPMs, and whether OBS cut the list short
/// (the search returns at most a fixed number of hits).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RpmSearch {
    pub rpms: Vec<CandidateRpm>,
    pub truncated: bool,
}

/// Whether `needle` can be put in a search expression as it is: the names
/// capabilities are made of, with no quote or operator in them.
pub fn is_searchable_needle(needle: &str) -> bool {
    !needle.is_empty()
        && needle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// The search request for RPMs of `arch` (or `noarch`) whose name contains
/// `needle`, in `projects`, or in every project when `projects` is empty.
pub fn rpm_search_path(needle: &str, projects: &[String], arch: &str) -> String {
    let quote = |s: &str| s.replace('\'', "");
    let mut expr = format!(
        "contains(@name,'{}') and (@arch='{}' or @arch='noarch')",
        quote(needle),
        quote(arch)
    );
    if !projects.is_empty() {
        let any: Vec<String> = projects
            .iter()
            .map(|p| format!("@project='{}'", quote(p)))
            .collect();
        expr.push_str(&format!(" and ({})", any.join(" or ")));
    }
    format!("/search/published/binary/id?match={}", enc(&expr))
}

/// Parsing half of [`search_rpms_containing`]: the binaries of a search
/// answer, and whether the answer was limited.
pub fn parse_rpm_search(xml: &str) -> RpmSearch {
    let rpms = xml
        .split("<binary ")
        .skip(1)
        .filter_map(|chunk| {
            let tag = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
            Some(CandidateRpm {
                name: crate::obs_xml::attr(tag, "name")?,
                project: crate::obs_xml::attr(tag, "project")?,
                repository: crate::obs_xml::attr(tag, "repository")?,
                arch: crate::obs_xml::attr(tag, "arch")?,
                package: crate::obs_xml::attr(tag, "package"),
            })
        })
        .collect();
    let head = xml.split('>').next().unwrap_or("");
    let truncated = crate::obs_xml::attr(head, "limited").as_deref() == Some("true");
    RpmSearch { rpms, truncated }
}

/// The RPMs of `arch` whose name contains `needle`, in `projects` (every
/// project when empty). Each RPM carries the file name the repository has it
/// under; the answer is limited by OBS and says so. The call is bounded by
/// [`OSC_QUERY_TIMEOUT`].
pub fn search_rpms_containing(
    needle: &str,
    projects: &[String],
    arch: &str,
    emit: Emit<'_>,
) -> Result<RpmSearch, Error> {
    if !is_searchable_needle(needle) {
        return Err(Error::NotActionable(format!(
            "{needle:?} cannot be searched for in a binary name"
        )));
    }
    let path = rpm_search_path(needle, projects, arch);
    let xml = api_get_within(&path, emit, OSC_QUERY_TIMEOUT)?;
    Ok(parse_rpm_search(&xml))
}

/// The `Provides:` of one RPM of a repository, each as the text OBS writes
/// (`pkgconfig(glib-2.0) = 2.80.0`, or the bare name).
pub fn parse_provides(xml: &str) -> Vec<String> {
    xml.split("<provides>")
        .skip(1)
        .filter_map(|chunk| chunk.split_once("</provides>"))
        .map(|(text, _)| crate::obs_xml::unescape(text.trim()))
        .collect()
}

/// A `Provides:` text split into the capability and its version, if it has
/// one: `pkgconfig(x) = 1.2` is (`pkgconfig(x)`, `=`, `1.2`).
pub fn split_provide(text: &str) -> (String, Option<(String, String)>) {
    let parts: Vec<&str> = text.split_whitespace().collect();
    match parts.as_slice() {
        [name, op, version, ..] if matches!(*op, "=" | "==" | ">=" | "<=" | ">" | "<") => (
            (*name).to_string(),
            Some(((*op).to_string(), (*version).to_string())),
        ),
        [name, ..] => ((*name).to_string(), None),
        [] => (String::new(), None),
    }
}

/// The `Provides:` of the binary `name` in `project`/`repository` for `arch`.
/// The repository names its files `<name>.rpm`, without the version and
/// release a search answer's file name carries: those are not addressable.
pub fn rpm_provides(
    project: &str,
    repository: &str,
    arch: &str,
    name: &str,
    emit: Emit<'_>,
) -> Result<Vec<String>, Error> {
    check_obs_name("project", project)?;
    let path = format!(
        "/build/{}/{}/{}/_repository/{}?view=fileinfo_ext",
        enc(project),
        enc(repository),
        enc(arch),
        enc(&format!("{name}.rpm"))
    );
    Ok(parse_provides(&api_get_within(
        &path,
        emit,
        OSC_QUERY_TIMEOUT,
    )?))
}

/// The `_meta` of a package, or `None` when the package does not exist.
pub fn get_package_meta(project: &str, pkg: &str, emit: Emit<'_>) -> Result<Option<String>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg)?;
    match api_get(&format!("/source/{project}/{}/_meta", enc(pkg)), emit) {
        Ok(meta) => Ok(Some(meta)),
        Err(Error::OscFailed { stderr, .. }) if is_not_found(&stderr) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The name of the binary a repository file names. A file at the top of the
/// listing is `<name>.rpm`. Some repositories also list their files under the
/// architecture directory, as `x86_64/<name>-<version>-<release>.<arch>.rpm`,
/// whose name is recovered by dropping the version, release and architecture;
/// the top-level `<name>.rpm` of the same binary reads the same.
fn listed_name(file: &str) -> Option<String> {
    let base = file.rsplit('/').next()?.strip_suffix(".rpm")?;
    if !file.contains('/') {
        return Some(base.to_string());
    }
    let (rest, _arch) = base.rsplit_once('.')?;
    let mut parts = rest.rsplitn(3, '-');
    let _release = parts.next()?;
    let _version = parts.next()?;
    parts.next().map(str::to_string)
}

/// The names of the binaries a repository lists (see [`listed_name`]), each
/// once. Only the top-level `<name>.rpm` form is read by [`rpm_provides`].
pub fn parse_repository_listing(xml: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for chunk in xml.split("<binary ").skip(1) {
        let tag = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
        let Some(file) = crate::obs_xml::attr(tag, "filename") else {
            continue;
        };
        let Some(name) = listed_name(&file) else {
            continue;
        };
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    names
}

/// The binary names a repository lists for `arch` (see
/// [`parse_repository_listing`]).
pub fn repository_names(
    project: &str,
    repository: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<Vec<String>, Error> {
    check_obs_name("project", project)?;
    let path = format!(
        "/build/{}/{}/{}/_repository",
        enc(project),
        enc(repository),
        enc(arch)
    );
    Ok(parse_repository_listing(&api_get(&path, emit)?))
}
