use super::*;

/// The attribute that marks a package (or project) as maintained.
pub const MAINTAINED_ATTRIBUTE: &str = "OBS:Maintained";

/// The attribute of a released project naming its update project.
pub const UPDATE_PROJECT_ATTRIBUTE: &str = "OBS:UpdateProject";

/// The attribute of the project maintenance requests go to.
pub const MAINTENANCE_PROJECT_ATTRIBUTE: &str = "OBS:MaintenanceProject";

/// Where a maintenance request is created; `addrevision` pins each source to
/// the revision it has now, as `osc maintenancerequest` does.
pub const CREATE_REQUEST_PATH: &str = "/request?cmd=create&addrevision=1";

pub(super) const OSC_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Code streams
// ---------------------------------------------------------------------------

/// One place a package is maintained in, as `osc maintained` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codestream {
    /// The update project, e.g. `SUSE:SLE-15-SP7:Update`.
    pub project: String,
    /// The package's name there.
    pub package: String,
    /// `using sources from PROJECT/PACKAGE`: the code stream takes this
    /// package from another one.
    pub devel: Option<(String, String)>,
}

/// The dry run of the attribute branch, which lists the maintained instances of
/// `pkg` without branching anything (what `osc maintained PKG` asks).
pub fn maintained_api_path(pkg: &str) -> String {
    format!(
        "/source?cmd=branch&attribute={}&update_project_attribute={}&dryrun=1&package={}",
        urlencoding::encode(MAINTAINED_ATTRIBUTE),
        urlencoding::encode(UPDATE_PROJECT_ATTRIBUTE),
        urlencoding::encode(pkg)
    )
}

/// The start tags of every `<name ...>` element in `xml`, without the name.
pub(super) fn start_tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name} ");
    xml.split(open.as_str())
        .skip(1)
        .map(|rest| &rest[..rest.find('>').unwrap_or(rest.len())])
        .collect()
}

/// The name a code stream lists a package under, without the incident suffix
/// (`curl.36123` is the incident copy of `curl`).
pub(super) fn without_incident_suffix(package: &str) -> &str {
    match package.rsplit_once('.') {
        Some((base, n))
            if !base.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => package,
    }
}

/// The code streams in the collection the attribute-branch dry run returns:
/// `<package project="P" package="N"><devel project=".." package=".."/>...`.
pub fn parse_maintained(xml: &str) -> Vec<Codestream> {
    let mut out: Vec<Codestream> = Vec::new();
    for chunk in xml.split("<package ").skip(1) {
        let head = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
        let (Some(project), Some(package)) = (
            crate::obs_xml::attr(head, "project"),
            crate::obs_xml::attr(head, "package"),
        ) else {
            continue;
        };
        let body = chunk.split("</package>").next().unwrap_or(chunk);
        let devel = start_tags(body, "devel").first().and_then(|t| {
            Some((
                crate::obs_xml::attr(t, "project")?,
                crate::obs_xml::attr(t, "package")?,
            ))
        });
        let cs = Codestream {
            project,
            package: without_incident_suffix(&package).to_string(),
            devel,
        };
        if !out
            .iter()
            .any(|c| c.project == cs.project && c.package == cs.package)
        {
            out.push(cs);
        }
    }
    out
}

/// The code streams in the text `osc maintained PKG` prints, one
/// `PROJECT/PACKAGE [using sources from P/N]` per line. A `[git] ...` line is a
/// branch on a git forge, which this tool does not submit to, and is skipped.
pub fn parse_maintained_text(text: &str) -> Vec<Codestream> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let (place, devel) = match line.split_once(" using sources from ") {
            Some((p, d)) => (p, d.trim().split_once('/')),
            None => (line, None),
        };
        let Some((project, package)) = place.trim().split_once('/') else {
            continue;
        };
        if !osc::is_valid_obs_name(project) || !osc::is_valid_obs_name(package) {
            continue;
        }
        out.push(Codestream {
            project: project.to_string(),
            package: without_incident_suffix(package).to_string(),
            devel: devel.map(|(p, n)| (p.to_string(), n.to_string())),
        });
    }
    out
}

/// A code stream maintained in git (SLFO and later), which `osc maintained`
/// prints as `[git] ORG/PACKAGE:BRANCH`. Not supported yet: listed and left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStream {
    pub org: String,
    pub package: String,
    pub branch: String,
}

impl std::fmt::Display for GitStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[git] {}/{}:{}", self.org, self.package, self.branch)
    }
}

/// The `[git] ORG/PACKAGE:BRANCH` lines of `osc maintained PKG`.
pub fn parse_git_maintained(text: &str) -> Vec<GitStream> {
    text.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("[git]")?.trim();
            let (org, rest) = rest.split_once('/')?;
            let (package, branch) = rest.rsplit_once(':')?;
            (!org.is_empty() && !package.is_empty() && !branch.is_empty()).then(|| GitStream {
                org: org.to_string(),
                package: package.to_string(),
                branch: branch.to_string(),
            })
        })
        .collect()
}

/// The git-maintained code streams of `pkg`: osc reads them from the
/// `packages.` service of the OBS instance, not from the API, so this runs
/// `osc maintained` itself. Read only. osc prints them after the OBS code
/// streams, so a package maintained only in git is not seen (osc stops at
/// "no packages found" first).
pub fn git_maintained(pkg: &str, emit: Emit<'_>) -> Result<Vec<GitStream>, Error> {
    if !osc::is_valid_obs_name(pkg) {
        return Err(Error::NotActionable(format!(
            "{pkg:?} is not a valid package name"
        )));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let out = proc::capture_with_timeout("osc", &["maintained", pkg], &cwd, emit, OSC_TIMEOUT)?;
    Ok(parse_git_maintained(&out.stdout))
}

/// Whether an OBS error says the attribute search found nothing: the package is
/// maintained nowhere, which is an answer, not a failure.
pub(super) fn says_nothing_maintained(stderr: &str) -> bool {
    stderr
        .to_lowercase()
        .contains("no packages found by search criteria")
}

/// Run `osc api`, with an optional method and body.
pub(super) fn api(
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    Ok(proc::capture_with_timeout(
        "osc",
        &args,
        &cwd,
        emit,
        OSC_TIMEOUT,
    )?)
}

pub(super) fn failed(cmd: String, out: Captured) -> Error {
    Error::OscFailed {
        cmd,
        code: out.code,
        stderr: out.stderr,
    }
}

/// The code streams that maintain `pkg`. Read only: the branch is a dry run.
pub fn maintained_codestreams(pkg: &str, emit: Emit<'_>) -> Result<Vec<Codestream>, Error> {
    if !osc::is_valid_obs_name(pkg) {
        return Err(Error::NotActionable(format!(
            "{pkg:?} is not a valid package name"
        )));
    }
    let path = maintained_api_path(pkg);
    let out = api(Some("POST"), &path, None, emit)?;
    if out.success() {
        return Ok(parse_maintained(&out.stdout));
    }
    if says_nothing_maintained(&out.stderr) || says_nothing_maintained(&out.stdout) {
        return Ok(Vec::new());
    }
    Err(failed(format!("api -X POST {path}"), out))
}

/// The search for the project maintenance requests go to.
pub fn maintenance_project_search_path() -> String {
    format!(
        "/search/project/id?match={}",
        urlencoding::encode(&format!(
            "attribute/@name='{MAINTENANCE_PROJECT_ATTRIBUTE}'"
        ))
    )
}

/// The first project of a search collection.
pub fn parse_first_project(xml: &str) -> Option<String> {
    start_tags(xml, "project")
        .first()
        .and_then(|t| crate::obs_xml::attr(t, "name"))
        .filter(|n| osc::is_valid_obs_name(n))
}

/// The maintenance project of the OBS instance (`SUSE:Maintenance`,
/// `openSUSE:Maintenance`), the way `osc maintenancerequest` finds it.
pub fn maintenance_project(emit: Emit<'_>) -> Result<String, Error> {
    let path = maintenance_project_search_path();
    let out = api(None, &path, None, emit)?;
    if !out.success() {
        return Err(failed(format!("api {path}"), out));
    }
    parse_first_project(&out.stdout).ok_or_else(|| {
        Error::NotActionable(format!(
            "no project on this OBS has the {MAINTENANCE_PROJECT_ATTRIBUTE} attribute; it takes no maintenance requests"
        ))
    })
}
