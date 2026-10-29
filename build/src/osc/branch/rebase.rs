//! Rebasing and deleting branches: the rebase request and its link checks, the
//! delete, and the local blocking of a package.

use super::*;

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
pub(super) fn linkinfo_error(xml: &str) -> Option<String> {
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
pub(super) fn linkinfo_is_behind(xml: &str) -> bool {
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
pub(super) fn block_locally(project: &str, emit: Emit<'_>) -> Result<(), Error> {
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
pub(super) fn parse_branch_response(
    xml: &str,
    source: &str,
    pkg: &str,
    target_project: &str,
) -> Branched {
    Branched {
        target_project: xml_data_value(xml, "targetproject")
            .unwrap_or_else(|| target_project.to_string()),
        target_package: xml_data_value(xml, "targetpackage").unwrap_or_else(|| pkg.to_string()),
        origin_project: xml_data_value(xml, "sourceproject").unwrap_or_else(|| source.to_string()),
    }
}
