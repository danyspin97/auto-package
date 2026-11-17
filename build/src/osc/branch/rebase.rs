//! Rebasing and deleting branches: the rebase request and its link checks, the
//! delete, and the local blocking of a package.

use super::*;
use crate::osc::branch::project_meta::*;
use crate::osc::error::*;
use crate::osc::names::*;
use crate::osc::query::*;
use ap_core::event::Event;

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
    let listing = api_get(&listing_path, emit)?;
    let seen = linkinfo_seen(&listing);
    emit.reporter.report(&Event::Progress {
        step: emit.step,
        message: format!("rebase {project}/{pkg}: {seen}"),
    });
    // A link whose base is already the origin's current revision has nothing
    // to rebase onto, and OBS answers the request with a 404 then.
    if !linkinfo_is_behind(&listing) {
        return check_link(project, pkg, emit);
    }
    let path = rebase_api_path(project, pkg);
    let out = rebase_post(&path, emit)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("api -X POST {path}"),
            code: out.code,
            stderr: rebase_failure_text(&out.stderr, &seen),
        });
    }
    check_link(project, pkg, emit)
}

/// The rebase request. It runs `osc --debug`: plain `osc api` prints only the
/// status line and the `<summary>` of an error, and the rest of OBS's answer
/// is only logged in debug mode. The request is sent once, never repeated.
fn rebase_post(path: &str, emit: Emit<'_>) -> Result<Captured, Error> {
    let args = ["--debug", "api", "-X", "POST", path];
    proc::capture_with_timeout("osc", &args, &anywhere(), emit, Duration::from_secs(120))
        .map_err(during(format!("api {path}")))
}

/// The text of a failed rebase request: osc's own lines (without its debug
/// log), the response body OBS sent, and the `<linkinfo>` values compared.
pub(super) fn rebase_failure_text(stderr: &str, seen: &str) -> String {
    let mut lines: Vec<String> = stderr
        .lines()
        .filter(|l| !l.contains("DEBUG:") && !l.trim().is_empty())
        .map(|l| l.trim_end().to_string())
        .collect();
    match obs_response_body(stderr) {
        Some(body) => lines.push(format!("OBS response body: {body}")),
        None => lines.push("OBS response body: not in osc's output".into()),
    }
    lines.push(seen.to_string());
    lines.join("\n")
}

/// The response body `osc --debug` logs for a failed request, as one line of
/// text trimmed to a few hundred characters. The log holds it as a Python
/// bytes literal (`DEBUG: b'<status code="...">\n ...'`), which is undone here.
pub(super) fn obs_response_body(stderr: &str) -> Option<String> {
    const LIMIT: usize = 300;
    let raw = stderr
        .lines()
        .rev()
        .find_map(|l| l.split_once("DEBUG: b'").map(|(_, rest)| rest))?
        .trim_end();
    let raw = raw.strip_suffix('\'')?;
    let mut text = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 't' | 'r') => text.push(' '),
            Some(other) => text.push(other),
            None => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= LIMIT {
        return Some(text);
    }
    let cut: String = text.chars().take(LIMIT).collect();
    Some(format!("{cut}..."))
}

/// The warning for a branch whose rebase failed and that is built as it is.
/// Its stored files are the old ones; the expanded view OBS builds still
/// merges the origin's current sources, so the two disagree.
pub fn stale_branch_warning(project: &str, pkg: &str, e: &Error) -> String {
    format!(
        "could not rebase {project}/{pkg} onto its origin: the branch's stored files are stale \
         (the expanded view OBS builds is still merged); building it as it is: {e}"
    )
}

/// The package `project/pkg` is linked from: the `project` and the `package` of
/// its `_link`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOrigin {
    pub project: String,
    pub package: String,
}

/// The origin `project/pkg` links to, when OBS positively names that origin as
/// missing: a read of the origin package answers `unknown_package` or
/// `unknown_project`. A read that succeeds, fails any other way, or a package
/// that is not a link gives `None`. Read only.
pub fn missing_origin(project: &str, pkg: &str, emit: Emit<'_>) -> Option<LinkOrigin> {
    let origin = link_origin(project, pkg, emit).ok().flatten()?;
    check_obs_name("project", &origin.project).ok()?;
    check_obs_name("package", &origin.package).ok()?;
    let path = format!("/source/{}/{}", origin.project, enc(&origin.package));
    let out = api_call(None, &path, None, emit).ok()?;
    let text = format!("{}{}", out.stdout, out.stderr);
    (!out.success() && names_missing(&text)).then_some(origin)
}

/// Whether OBS's answer names a package or a project as missing.
pub(super) fn names_missing(text: &str) -> bool {
    [
        "unknown_package",
        "unknown_project",
        "Package not found",
        "Project not found",
    ]
    .iter()
    .any(|m| text.contains(m))
}

/// The project `project/pkg` is linked from (the `project` of its `_link`), or
/// `None` when it is not a link.
pub fn link_project(project: &str, pkg: &str, emit: Emit<'_>) -> Result<Option<String>, Error> {
    Ok(link_origin(project, pkg, emit)?.map(|origin| origin.project))
}

/// The origin `project/pkg` is linked from, or `None` when it is not a link. A
/// `_link` that names no package is a link to `pkg` itself.
pub fn link_origin(project: &str, pkg: &str, emit: Emit<'_>) -> Result<Option<LinkOrigin>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg)?;
    let path = format!("/source/{project}/{}/_link", enc(pkg));
    let link = api_call(None, &path, None, emit)?;
    if !link.success() {
        if is_not_found(&link.stderr) {
            return Ok(None);
        }
        return Err(Error::OscFailed {
            cmd: format!("api {path}"),
            code: link.code,
            stderr: link.stderr,
        });
    }
    Ok(parse_link_origin(&link.stdout, pkg))
}

/// The origin a `_link` file names, `None` when it names no project.
pub(super) fn parse_link_origin(xml: &str, pkg: &str) -> Option<LinkOrigin> {
    let project = crate::obs_xml::attr(xml, "project").filter(|p| !p.is_empty())?;
    let package = crate::obs_xml::attr(xml, "package")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| pkg.to_string());
    Some(LinkOrigin { project, package })
}

#[cfg(test)]
mod link_origin_tests {
    use super::*;

    #[test]
    fn a_link_names_its_origin_project_and_package() {
        let xml = r#"<link project="Factory" package="hyprlang-src"/>"#;
        assert_eq!(
            parse_link_origin(xml, "hyprlang"),
            Some(LinkOrigin {
                project: "Factory".into(),
                package: "hyprlang-src".into(),
            })
        );
    }

    #[test]
    fn a_link_without_a_package_points_at_the_same_name() {
        let xml = r#"<link project="openSUSE:Factory"/>"#;
        assert_eq!(
            parse_link_origin(xml, "hyprlang"),
            Some(LinkOrigin {
                project: "openSUSE:Factory".into(),
                package: "hyprlang".into(),
            })
        );
    }

    #[test]
    fn a_link_with_no_project_is_no_origin() {
        assert_eq!(parse_link_origin(r#"<link package="x"/>"#, "x"), None);
        assert_eq!(parse_link_origin(r#"<link project=""/>"#, "x"), None);
    }
}

/// The `srcmd5` and `baserev` of a package listing's `<linkinfo>`, each `None`
/// when absent (or when there is no `<linkinfo>` at all).
fn linkinfo_revs(xml: &str) -> (Option<String>, Option<String>) {
    let Some(rest) = xml.find("<linkinfo").map(|i| &xml[i..]) else {
        return (None, None);
    };
    let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
    (
        crate::obs_xml::attr(tag, "srcmd5"),
        crate::obs_xml::attr(tag, "baserev"),
    )
}

/// Whether a package listing's `<linkinfo>` shows the link based on a revision
/// of its origin other than the current one. Unknown counts as behind, so the
/// rebase is still asked for.
pub(super) fn linkinfo_is_behind(xml: &str) -> bool {
    match linkinfo_revs(xml) {
        (Some(current), Some(base)) => current != base,
        _ => true,
    }
}

/// The `<linkinfo>` values [`rebase_package`] compared, as one line for the
/// run record and for a 404 from the rebase request.
fn linkinfo_seen(xml: &str) -> String {
    let (srcmd5, baserev) = linkinfo_revs(xml);
    let shown = |v: Option<String>| v.unwrap_or_else(|| "none".into());
    format!(
        "linkinfo srcmd5={} baserev={} behind={}",
        shown(srcmd5),
        shown(baserev),
        linkinfo_is_behind(xml)
    )
}

#[cfg(test)]
mod linkinfo_seen_tests {
    use super::*;

    #[test]
    fn the_values_compared_are_recorded_with_the_verdict() {
        let at = r#"<directory><linkinfo project="P" srcmd5="aa" baserev="aa"/></directory>"#;
        assert_eq!(
            linkinfo_seen(at),
            "linkinfo srcmd5=aa baserev=aa behind=false"
        );
        let behind = r#"<directory><linkinfo project="P" srcmd5="bb" baserev="aa"/></directory>"#;
        assert_eq!(
            linkinfo_seen(behind),
            "linkinfo srcmd5=bb baserev=aa behind=true"
        );
    }

    #[test]
    fn a_missing_value_is_recorded_as_none_and_counts_as_behind() {
        assert_eq!(
            linkinfo_seen(r#"<linkinfo project="P" srcmd5="aa"/>"#),
            "linkinfo srcmd5=aa baserev=none behind=true"
        );
        assert_eq!(
            linkinfo_seen("<directory/>"),
            "linkinfo srcmd5=none baserev=none behind=true"
        );
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
    for start in repository_elements(xml)
        .ok()?
        .into_iter()
        .map(|r| r.start)
        .rev()
    {
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
    let _meta = lock_project_meta();
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

#[cfg(test)]
mod missing_origin_tests {
    use super::*;

    #[test]
    fn only_a_positive_missing_answer_names_the_origin_as_missing() {
        assert!(names_missing(
            r#"<status code="unknown_package"><summary>Package not found: Base:System/dtc</summary></status>"#
        ));
        assert!(names_missing("<status code=\"unknown_project\"/>"));
        // A proxy or a web server's own 404 page is not OBS's answer.
        assert!(!names_missing(
            "<html><h1>404 Not Found</h1><hr>nginx</html>"
        ));
        assert!(!names_missing(""));
    }
}

#[cfg(test)]
mod rebase_failure_tests {
    use super::*;

    /// What `osc --debug api -X POST` prints for a 404 whose body has a summary.
    const DEBUG_404: &str = "Server returned an error: HTTP Error 404: Not Found\n\
        Package not found: home:u:branches:S/p\n\
        [2026-11-17 14:10:12] DEBUG: HTTPHeaderDict({'status': '404 Not Found', 'x-opensuse-errorcode': 'unknown_package'})\n\
        [2026-11-17 14:10:12] DEBUG: b'<status code=\"unknown_package\">\\n  <summary>Package not found: home:u:branches:S/p</summary>\\n</status>\\n'\n";

    #[test]
    fn the_body_is_read_from_the_debug_log_and_unescaped() {
        assert_eq!(
            obs_response_body(DEBUG_404).as_deref(),
            Some(
                "<status code=\"unknown_package\"> <summary>Package not found: home:u:branches:S/p</summary> </status>"
            )
        );
    }

    #[test]
    fn a_body_longer_than_the_limit_is_cut() {
        let long = "x".repeat(500);
        let stderr = format!("DEBUG: b'{long}'\n");
        let body = obs_response_body(&stderr).unwrap();
        assert_eq!(body.chars().count(), 303);
        assert!(body.ends_with("..."));
    }

    #[test]
    fn no_debug_body_is_none() {
        assert_eq!(
            obs_response_body("Server returned an error: HTTP Error 500"),
            None
        );
    }

    #[test]
    fn the_failure_text_keeps_osc_lines_and_the_body_without_the_debug_log() {
        let text = rebase_failure_text(DEBUG_404, "linkinfo srcmd5=aa baserev=bb behind=true");
        assert_eq!(
            text,
            "Server returned an error: HTTP Error 404: Not Found\n\
             Package not found: home:u:branches:S/p\n\
             OBS response body: <status code=\"unknown_package\"> <summary>Package not found: home:u:branches:S/p</summary> </status>\n\
             linkinfo srcmd5=aa baserev=bb behind=true"
        );
        assert!(is_not_found(&text));
    }

    #[test]
    fn the_warning_says_the_stored_files_are_stale_and_carries_the_body() {
        let e = Error::OscFailed {
            cmd: "api -X POST /source/B/p?cmd=rebase".into(),
            code: 1,
            stderr: rebase_failure_text(DEBUG_404, "linkinfo srcmd5=aa baserev=bb behind=true"),
        };
        let msg = stale_branch_warning("B", "p", &e);
        assert!(
            msg.starts_with("could not rebase B/p onto its origin"),
            "{msg}"
        );
        assert!(msg.contains("the branch's stored files are stale"), "{msg}");
        assert!(
            msg.contains("the expanded view OBS builds is still merged"),
            "{msg}"
        );
        assert!(msg.contains("building it as it is"), "{msg}");
        assert!(msg.contains("<summary>Package not found"), "{msg}");
    }
}
