use super::*;

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// One code stream of a maintenance request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentAction {
    /// The branch project the change was built in.
    pub source_project: String,
    pub source_package: String,
    /// The update project the incident releases into.
    pub release_project: String,
}

/// A request to start a maintenance incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceRequest {
    /// The maintenance project (`SUSE:Maintenance`).
    pub target_project: String,
    pub actions: Vec<IncidentAction>,
    /// `cleanup`: remove the branch once the incident is accepted.
    pub sourceupdate: Option<String>,
    pub description: String,
}

/// The document `POST /request?cmd=create` takes for `req`: one
/// `maintenance_incident` action per code stream, as `osc maintenancerequest`
/// writes it.
pub fn request_xml(req: &MaintenanceRequest) -> String {
    let mut x = String::from("<request>\n");
    for a in &req.actions {
        x.push_str("  <action type=\"maintenance_incident\">\n");
        x.push_str(&format!(
            "    <source project=\"{}\" package=\"{}\"/>\n",
            crate::obs_xml::escape(&a.source_project),
            crate::obs_xml::escape(&a.source_package)
        ));
        x.push_str(&format!(
            "    <target project=\"{}\" releaseproject=\"{}\"/>\n",
            crate::obs_xml::escape(&req.target_project),
            crate::obs_xml::escape(&a.release_project)
        ));
        if let Some(s) = &req.sourceupdate {
            x.push_str(&format!(
                "    <options>\n      <sourceupdate>{}</sourceupdate>\n    </options>\n",
                crate::obs_xml::escape(s)
            ));
        }
        x.push_str("  </action>\n");
    }
    x.push_str(&format!(
        "  <description>{}</description>\n",
        crate::obs_xml::escape(&req.description)
    ));
    x.push_str("</request>\n");
    x
}

/// Create the request and return its id.
pub fn create_request(req: &MaintenanceRequest, emit: Emit<'_>) -> Result<String, Error> {
    if req.actions.is_empty() {
        return Err(Error::NotActionable(
            "a maintenance request needs at least one code stream".into(),
        ));
    }
    for name in std::iter::once(&req.target_project).chain(
        req.actions
            .iter()
            .flat_map(|a| [&a.source_project, &a.source_package, &a.release_project]),
    ) {
        if !osc::is_valid_obs_name(name) {
            return Err(Error::NotActionable(format!(
                "refusing to use {name:?}: not a valid OBS name"
            )));
        }
    }
    let out = api(
        Some("POST"),
        CREATE_REQUEST_PATH,
        Some(&request_xml(req)),
        emit,
    )?;
    if !out.success() {
        return Err(failed(format!("api -X POST {CREATE_REQUEST_PATH}"), out));
    }
    osc::parse_request_id(&out.stdout).ok_or_else(|| {
        Error::NotActionable(format!(
            "OBS accepted the request but returned no request id: {}",
            out.stdout.lines().next().unwrap_or("")
        ))
    })
}

/// A maintenance request found on the server for a branch package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenIncident {
    pub id: String,
    pub state: String,
    pub source_project: String,
    pub source_package: String,
    pub release_project: Option<String>,
    /// The source revision the request was pinned to (`addrevision`): the
    /// expanded srcmd5 (`xsrcmd5`) of a link, the srcmd5 or revision otherwise.
    pub rev: Option<String>,
}

/// The query for the maintenance requests whose source is `project/package`
/// that already took a change: open (`new`, `review`) or `accepted`. A
/// revoked, declined or superseded request does not count.
pub fn open_incidents_path(project: &str, package: &str) -> String {
    format!(
        "/request?view=collection&roles=source&types=maintenance_incident&states=new,review,accepted&project={}&package={}",
        urlencoding::encode(project),
        urlencoding::encode(package)
    )
}

/// Every name the current source of a package goes by in a request's `rev`:
/// the revision number, the srcmd5 and, for a link, the expanded `xsrcmd5`
/// (what `addrevision` pins a branch to).
pub fn parse_source_revisions(directory_xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(dir) = start_tags(directory_xml, "directory").first() {
        out.extend(crate::obs_xml::attr(dir, "rev"));
        out.extend(crate::obs_xml::attr(dir, "srcmd5"));
    }
    if let Some(link) = start_tags(directory_xml, "linkinfo").first() {
        out.extend(crate::obs_xml::attr(link, "xsrcmd5"));
    }
    out.retain(|r| !r.is_empty());
    out.dedup();
    out
}

/// [`parse_source_revisions`] of `project/package` on the server.
pub fn source_revisions(
    project: &str,
    package: &str,
    emit: Emit<'_>,
) -> Result<Vec<String>, Error> {
    for name in [project, package] {
        if !osc::is_valid_obs_name(name) {
            return Err(Error::NotActionable(format!(
                "refusing to use {name:?}: not a valid OBS name"
            )));
        }
    }
    let path = format!(
        "/source/{}/{}",
        urlencoding::encode(project),
        urlencoding::encode(package)
    );
    let out = api(None, &path, None, emit)?;
    if !out.success() {
        return Err(failed(format!("api {path}"), out));
    }
    Ok(parse_source_revisions(&out.stdout))
}

/// The `maintenance_incident` actions of every request in a collection.
pub fn parse_open_incidents(xml: &str) -> Vec<OpenIncident> {
    let mut out = Vec::new();
    for req in xml.split("<request ").skip(1) {
        let req = req.split("</request>").next().unwrap_or(req);
        let head = &req[..req.find('>').unwrap_or(req.len())];
        let Some(id) = crate::obs_xml::attr(head, "id") else {
            continue;
        };
        let state = start_tags(req, "state")
            .first()
            .and_then(|t| crate::obs_xml::attr(t, "name"))
            .unwrap_or_default();
        for action in req.split("<action ").skip(1) {
            let action = action.split("</action>").next().unwrap_or(action);
            let head = &action[..action.find('>').unwrap_or(action.len())];
            if crate::obs_xml::attr(head, "type").as_deref() != Some("maintenance_incident") {
                continue;
            }
            let Some(source) = start_tags(action, "source").into_iter().next() else {
                continue;
            };
            let (Some(sp), Some(spkg)) = (
                crate::obs_xml::attr(source, "project"),
                crate::obs_xml::attr(source, "package"),
            ) else {
                continue;
            };
            // Once accepted, `project` names the incident (`openSUSE:Maintenance:123`);
            // `releaseproject` stays.
            let release = start_tags(action, "target")
                .first()
                .and_then(|t| crate::obs_xml::attr(t, "releaseproject"));
            out.push(OpenIncident {
                id: id.clone(),
                state: state.clone(),
                source_project: sp,
                source_package: spkg,
                release_project: release,
                rev: crate::obs_xml::attr(source, "rev").filter(|r| !r.is_empty()),
            });
        }
    }
    out
}

/// The request that already took `project/package` into `release`: an open
/// one whatever its revision, or an accepted one pinned to a revision the
/// branch package still has (`current`, see [`parse_source_revisions`]). A
/// request without a release project counts, since OBS then takes the one the
/// branch links to.
pub fn find_open_incident<'a>(
    open: &'a [OpenIncident],
    project: &str,
    package: &str,
    release: &str,
    current: &[String],
) -> Option<&'a OpenIncident> {
    open.iter().find(|o| {
        o.source_project == project
            && o.source_package == package
            && o.release_project.as_deref().is_none_or(|r| r == release)
            && match o.state.as_str() {
                "accepted" => o.rev.as_ref().is_some_and(|r| current.contains(r)),
                _ => true,
            }
    })
}

/// Why `project/package` already went to `release`: `request N is already
/// open`, or `request N already submitted this revision (accepted)`.
pub fn submitted_reason(o: &OpenIncident, branch: &str) -> String {
    if o.state == "accepted" {
        format!(
            "request {} already submitted this revision of {branch} (accepted)",
            o.id
        )
    } else {
        format!("request {} from {branch} is already open", o.id)
    }
}

/// The maintenance request from `project/package` into `release` that already
/// took the change (see [`find_open_incident`]), with why.
pub fn open_incident(
    project: &str,
    package: &str,
    release: &str,
    emit: Emit<'_>,
) -> Result<Option<String>, Error> {
    let path = open_incidents_path(project, package);
    let out = api(None, &path, None, emit)?;
    if !out.success() {
        return Err(failed(format!("api {path}"), out));
    }
    let open = parse_open_incidents(&out.stdout);
    // The accepted ones only count at the revision the branch has now; one
    // that cannot be read leaves only the open ones counting.
    let current = if open.iter().any(|o| o.state == "accepted") {
        source_revisions(project, package, emit).unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(
        find_open_incident(&open, project, package, release, &current)
            .map(|o| submitted_reason(o, project)),
    )
}

/// The description of a maintenance request: what kind of update it is
/// (`mode_line`), then the newest changelog entry of the change (SUSE's
/// maintenance tools read the bug references out of it), once when every code
/// stream has the same, else per code stream; the references it does not
/// mention follow on a line of their own.
pub fn request_description(
    pkg: &str,
    mode_line: &str,
    entries: &[(String, Option<String>)],
    refs: &[String],
) -> String {
    let bodies: Vec<&str> = entries.iter().filter_map(|(_, b)| b.as_deref()).collect();
    let body = if bodies.is_empty() {
        format!("Maintenance update of {pkg}.")
    } else if bodies.len() == entries.len() && bodies.iter().all(|b| *b == bodies[0]) {
        bodies[0].to_string()
    } else {
        entries
            .iter()
            .map(|(cs, b)| format!("{cs}:\n{}", b.as_deref().unwrap_or("(no changelog entry)")))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let mut out = if mode_line.is_empty() {
        body
    } else {
        format!("{mode_line}\n\n{body}")
    };
    let missing = missing_refs(&out, refs);
    if !missing.is_empty() {
        out.push_str(&format!("\n\nReferences: {}", missing.join(", ")));
    }
    out.push_str("\n\nBuilt and submitted by auto-package.");
    out
}

/// The plan shown before a maintenance request is created: the mode, every
/// code stream in it with how its change went (`notes`, by release project),
/// and every code stream left out with why.
pub fn plan_lines(
    req: &MaintenanceRequest,
    refs: &[String],
    mode_line: &str,
    notes: &[(String, String)],
    skipped: &[(String, String)],
) -> Vec<String> {
    let mut out = vec![format!(
        "maintenance request to {} ({} code stream{}):",
        req.target_project,
        req.actions.len(),
        if req.actions.len() == 1 { "" } else { "s" }
    )];
    if !mode_line.is_empty() {
        out.push(format!("  mode: {mode_line}"));
    }
    for a in &req.actions {
        let note = notes
            .iter()
            .find(|(r, _)| *r == a.release_project)
            .map(|(_, n)| format!(": {n}"))
            .unwrap_or_default();
        out.push(format!(
            "  {}/{} -> release in {}{note}",
            a.source_project, a.source_package, a.release_project
        ));
    }
    for (cs, why) in skipped {
        out.push(format!("  {cs}: left out, {why}"));
    }
    out.push(format!(
        "  bug references: {}",
        if refs.is_empty() {
            "none (--no-bug-ref)".to_string()
        } else {
            refs.join(", ")
        }
    ));
    out.push(format!(
        "  on accept: {}",
        match req.sourceupdate.as_deref() {
            Some("cleanup") => "the branch package is removed",
            _ => "the branch package is kept",
        }
    ));
    out.push("  description:".to_string());
    out.extend(req.description.lines().map(|l| format!("    | {l}")));
    out
}
