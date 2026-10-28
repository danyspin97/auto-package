//! Maintenance requests for SLE and Leap code streams.
//!
//! A package that is maintained in a released code stream (SUSE:SLE-15-SP7:Update,
//! openSUSE:Leap:15.6:Update, ...) is not changed with a submit request to its
//! origin. The change goes to the maintenance project instead, which opens an
//! incident and releases it into the code stream. This is what `osc mbranch` and
//! `osc maintenancerequest` do by hand; here it rides on `update` and `lint`:
//!
//! 1. Which code streams maintain the package: the dry run of the attribute
//!    branch `osc maintained` makes,
//!    `POST /source?cmd=branch&attribute=OBS:Maintained&update_project_attribute=OBS:UpdateProject&dryrun=1&package=PKG`,
//!    which lists each update project with the package.
//! 2. For every chosen code stream, the ordinary `update`/`lint` run with the
//!    update project as its source and `home:<user>:branches:<update project>`
//!    as its branch project (what `osc branch <update project> PKG` names it).
//!    That run branches the package and builds the change there; there is no
//!    second build pipeline. `osc mbranch` would put every code stream into one
//!    `home:<user>:branches:OBS_Maintained:PKG` project with packages renamed
//!    `PKG.<update project>`, which the pipeline cannot build (it needs the
//!    package under its own name, in a project with one build target), so each
//!    code stream gets its own branch project, as `osc branch` makes them.
//! 3. The bug references go into the newest `.changes` entry, the way SUSE
//!    writes them: `- Update to 1.2.3 (bsc#123456, CVE-2025-1234):`.
//! 4. One `maintenance_incident` request per package, with one action per code
//!    stream (source: the branch package; target: the project with the
//!    `OBS:MaintenanceProject` attribute, `releaseproject`: the update project),
//!    created with `POST /request?cmd=create&addrevision=1`, which is the request
//!    `osc maintenancerequest BRANCH PKG UPDATEPROJECT` creates. A code stream
//!    whose branch package already has an open request, or an accepted one of
//!    the same source revision, is left out of it.
//!
//! A run is in one [`Mode`]: `backport` (the default: each code stream keeps
//! its version and gets the upstream fix of `--backport` as a patch, see
//! [`crate::backport`]), `update` (each code stream gets the version `update`
//! builds) or `lint` (packaging fixes). Every code stream is built from its own
//! package and reported on its own ([`StreamReport`]): one that fails is left
//! out of the request with the reason. Which code streams a fix affects can be
//! decided by an external tool through [`StreamCheck`] (`--affected-by`).
//! Code streams maintained in git (SLFO, `[git]` in `osc maintained`) are
//! listed and left out: they are not supported yet.
//!
//! Everything that builds a query, a document or a text is a pure function with
//! its own tests; the wrappers that run `osc` need a live OBS and are not covered.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::backport;
use crate::osc::{self, Error};
use crate::proc::{self, Captured, Emit};

/// The attribute that marks a package (or project) as maintained.
pub const MAINTAINED_ATTRIBUTE: &str = "OBS:Maintained";
/// The attribute of a released project naming its update project.
pub const UPDATE_PROJECT_ATTRIBUTE: &str = "OBS:UpdateProject";
/// The attribute of the project maintenance requests go to.
pub const MAINTENANCE_PROJECT_ATTRIBUTE: &str = "OBS:MaintenanceProject";
/// Where a maintenance request is created; `addrevision` pins each source to
/// the revision it has now, as `osc maintenancerequest` does.
pub const CREATE_REQUEST_PATH: &str = "/request?cmd=create&addrevision=1";

const OSC_TIMEOUT: Duration = Duration::from_secs(120);

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
fn start_tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name} ");
    xml.split(open.as_str())
        .skip(1)
        .map(|rest| &rest[..rest.find('>').unwrap_or(rest.len())])
        .collect()
}

/// The name a code stream lists a package under, without the incident suffix
/// (`curl.36123` is the incident copy of `curl`).
fn without_incident_suffix(package: &str) -> &str {
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
fn says_nothing_maintained(stderr: &str) -> bool {
    stderr
        .to_lowercase()
        .contains("no packages found by search criteria")
}

/// Run `osc api`, with an optional method and body.
fn api(
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

fn failed(cmd: String, out: Captured) -> Error {
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

/// The code streams of `available` that `wanted` names. A name is an update
/// project as `osc maintained` prints it, or the same without `:Update`
/// (`SUSE:SLE-15-SP7` for `SUSE:SLE-15-SP7:Update`); `all` takes every one. A
/// name that maintains nothing is refused, with the ones that would do.
pub fn select_codestreams(
    available: &[Codestream],
    wanted: &[String],
) -> Result<Vec<Codestream>, String> {
    let wanted: Vec<&str> = wanted
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect();
    if wanted.iter().any(|w| w.eq_ignore_ascii_case("all")) {
        return Ok(available.to_vec());
    }
    let mut out: Vec<Codestream> = Vec::new();
    let mut refused: Vec<&str> = Vec::new();
    for w in wanted {
        let found = available.iter().find(|c| c.project == w).or_else(|| {
            available
                .iter()
                .find(|c| c.project == format!("{w}:Update"))
        });
        match found {
            Some(c) if !out.contains(c) => out.push(c.clone()),
            Some(_) => {}
            None => refused.push(w),
        }
    }
    if !refused.is_empty() {
        let known: Vec<&str> = available.iter().map(|c| c.project.as_str()).collect();
        return Err(format!(
            "{} {} not maintain the package; it is maintained in: {}",
            refused.join(", "),
            if refused.len() == 1 { "does" } else { "do" },
            if known.is_empty() {
                "nothing".to_string()
            } else {
                known.join(", ")
            }
        ));
    }
    if out.is_empty() {
        return Err("no code stream chosen".to_string());
    }
    Ok(out)
}

/// The code streams every package of `per_package` is maintained in, in the
/// order of the first.
pub fn common_codestreams(per_package: &[Vec<Codestream>]) -> Vec<String> {
    let Some(first) = per_package.first() else {
        return Vec::new();
    };
    first
        .iter()
        .map(|c| c.project.clone())
        .filter(|p| {
            per_package
                .iter()
                .all(|cs| cs.iter().any(|c| &c.project == p))
        })
        .fold(Vec::new(), |mut acc, p| {
            if !acc.contains(&p) {
                acc.push(p);
            }
            acc
        })
}

/// The numbered list the user picks code streams from.
pub fn pick_lines(projects: &[String]) -> Vec<String> {
    projects
        .iter()
        .enumerate()
        .map(|(i, p)| format!("  {}) {p}", i + 1))
        .collect()
}

/// The code streams a typed answer picks out of `projects`: numbers from
/// [`pick_lines`] or project names, separated by commas or spaces, or `all`.
pub fn parse_pick(answer: &str, projects: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let words: Vec<&str> = answer
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return Err("nothing picked".to_string());
    }
    for w in words {
        if w.eq_ignore_ascii_case("all") {
            return Ok(projects.to_vec());
        }
        let picked = match w.parse::<usize>() {
            Ok(n) if (1..=projects.len()).contains(&n) => projects[n - 1].clone(),
            Ok(n) => return Err(format!("{n} is not in the list (1-{})", projects.len())),
            Err(_) => match projects
                .iter()
                .find(|p| *p == w || **p == format!("{w}:Update"))
            {
                Some(p) => p.clone(),
                None => return Err(format!("{w} is not in the list")),
            },
        };
        if !out.contains(&picked) {
            out.push(picked);
        }
    }
    Ok(out)
}

/// The projects an update project's meta links to (`<link project="GA"/>`):
/// where its packages come from until a first update.
pub fn parse_project_links(meta: &str) -> Vec<String> {
    let head_end = meta.find("<repository").unwrap_or(meta.len());
    start_tags(&meta[..head_end], "link")
        .into_iter()
        .filter_map(|t| crate::obs_xml::attr(t, "project"))
        .collect()
}

/// The project `update` browses for a code stream: the update project when it
/// has the package itself, else the project it links to (a package not updated
/// since the release lives there; OBS branches it to the update project).
pub fn planning_source(
    update_project: &str,
    update_lists_package: bool,
    update_meta: &str,
) -> Option<String> {
    if update_lists_package {
        return Some(update_project.to_string());
    }
    parse_project_links(update_meta).into_iter().next()
}

/// The newest incident copy of `pkg` (`pkg.NNNNN`, highest number) among the
/// package names of an update project.
pub fn newest_incident_in<'a>(
    listing: impl IntoIterator<Item = &'a str>,
    pkg: &str,
) -> Option<String> {
    let prefix = format!("{pkg}.");
    listing
        .into_iter()
        .filter_map(|name| {
            let n = name.strip_prefix(&prefix)?;
            (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| (n.parse::<u64>().ok(), name))
        })
        .filter_map(|(n, name)| Some((n?, name)))
        .max_by_key(|(n, _)| *n)
        .map(|(_, name)| name.to_string())
}

/// [`newest_incident_in`] for `project`'s listing; `None` for a name that is
/// an incident copy itself, or a listing that cannot be read.
pub fn newest_incident_package(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    if without_incident_suffix(pkg) != pkg {
        return None;
    }
    let listing = osc::try_list_project_packages(project, emit).ok()?;
    newest_incident_in(listing.iter().map(String::as_str), pkg)
}

/// Whether a source project of this kind may be branched from in a maintenance
/// run: an update project is a `maintenance_release` project.
pub fn source_kind_allowed(kind: &str) -> bool {
    kind == "maintenance_release"
}

/// The branch project of a code stream: `home:<user>:branches:<update project>`,
/// what `osc branch` names it.
pub fn branch_project_for(user: &str, update_project: &str) -> String {
    osc::default_branch_project(user, update_project)
}

/// `cleanup` when the branch is one of the user's own `home:<user>:branches:`
/// projects, so OBS removes it once the incident is accepted (`osc
/// maintenancerequest` decides the same way); otherwise nothing.
pub fn sourceupdate_for(branch: &str, user: Option<&str>) -> Option<&'static str> {
    let user = user?;
    branch
        .starts_with(&format!("home:{user}:branches:"))
        .then_some("cleanup")
}

// ---------------------------------------------------------------------------
// Bug references
// ---------------------------------------------------------------------------

static BUG_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^(bsc|boo|bnc)#([0-9]{4,8})$").expect("valid regex"));
static JIRA_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^jsc#([a-z][a-z0-9]*)-([0-9]+)$").expect("valid regex")
});
static CVE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^cve-((?:19|20)[0-9]{2})-([0-9]{4,7})$").expect("valid regex")
});

/// A bug reference in the spelling SUSE changelogs use (`bsc#1234567`,
/// `jsc#SLE-12345`, `CVE-2025-1234`), or why `s` is not one.
pub fn parse_bug_ref(s: &str) -> Result<String, String> {
    let s = s.trim();
    if let Some(c) = BUG_RE.captures(s) {
        return Ok(format!("{}#{}", c[1].to_lowercase(), &c[2]));
    }
    if let Some(c) = JIRA_RE.captures(s) {
        return Ok(format!("jsc#{}-{}", c[1].to_uppercase(), &c[2]));
    }
    if let Some(c) = CVE_RE.captures(s) {
        return Ok(format!("CVE-{}-{}", &c[1], &c[2]));
    }
    Err(format!(
        "{s:?} is not a bug reference: use bsc#NNNNNNN (also boo#, bnc#), jsc#PROJECT-NNN or CVE-YYYY-NNNN"
    ))
}

/// Whether a reference names a CVE.
pub fn is_cve(r: &str) -> bool {
    r.starts_with("CVE-")
}

/// The references of `--bug`, checked and in SUSE spelling, each once.
/// Without any, a maintenance request is refused unless `no_bug_ref`.
pub fn parse_bug_refs(given: &[String], no_bug_ref: bool) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for g in given
        .iter()
        .flat_map(|g| g.split(','))
        .map(str::trim)
        .filter(|g| !g.is_empty())
    {
        let r = parse_bug_ref(g)?;
        if !out.contains(&r) {
            out.push(r);
        }
    }
    if out.is_empty() && !no_bug_ref {
        return Err(
            "a maintenance update needs a bug reference: pass --bug bsc#NNNNNNN (and --bug CVE-YYYY-NNNN for a \
             security fix), or --no-bug-ref to submit without one"
                .to_string(),
        );
    }
    Ok(out)
}

/// What to say about a set of references that is valid but unusual: a CVE
/// without the bsc# bug that tracks it.
pub fn bug_ref_warnings(refs: &[String]) -> Vec<String> {
    let has_bug = refs
        .iter()
        .any(|r| r.starts_with("bsc#") || r.starts_with("boo#") || r.starts_with("bnc#"));
    if refs.iter().any(|r| is_cve(r)) && !has_bug {
        vec![
            "a CVE is fixed but no bsc# bug is given; SUSE tracks every CVE with a bsc# bug"
                .to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// `(bsc#1, CVE-2025-1)`.
pub fn refs_suffix(refs: &[String]) -> String {
    format!("({})", refs.join(", "))
}

/// Whether `text` mentions `r` as a whole reference (`bsc#12` is not in `bsc#123`).
fn mentions(text: &str, r: &str) -> bool {
    let lower = text.to_lowercase();
    let r = r.to_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&r) {
        let end = from + i + r.len();
        let before_ok = from + i == 0 || !lower.as_bytes()[from + i - 1].is_ascii_alphanumeric();
        let after_ok = lower
            .as_bytes()
            .get(end)
            .is_none_or(|b| !b.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// The references of `refs` that `text` does not mention yet.
pub fn missing_refs(text: &str, refs: &[String]) -> Vec<String> {
    refs.iter()
        .filter(|r| !mentions(text, r))
        .cloned()
        .collect()
}

fn is_separator(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// `changes` with the references its newest entry lacks added to the end of
/// that entry's first bullet, before a closing `:`, the way SUSE writes them:
/// `- Update to 1.2.3 (bsc#123456, CVE-2025-1234):`. An entry with no bullet
/// gets a `- References: ...` one. `None` when the entry already names them
/// all, or there is no entry.
pub fn with_refs_in_newest_entry(changes: &str, refs: &[String]) -> Option<String> {
    let lines: Vec<&str> = changes.lines().collect();
    let first = lines.iter().position(|l| is_separator(l))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l))
        .map_or(lines.len(), |i| first + 1 + i);
    // The header `date - author <mail>`, then the body.
    let header = (first + 1..end).find(|&i| !lines[i].trim().is_empty())?;
    let entry = lines[header + 1..end].join("\n");
    let missing = missing_refs(&entry, refs);
    if missing.is_empty() {
        return None;
    }
    let suffix = refs_suffix(&missing);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let bullet = (header + 1..end).find(|&i| lines[i].trim_start().starts_with("- "));
    match bullet {
        Some(b) => {
            // The bullet's text runs on over indented lines that do not start a
            // sub-list of their own.
            let mut last = b;
            for (i, line) in lines.iter().enumerate().take(end).skip(b + 1) {
                let t = line.trim_start();
                let continues =
                    line.starts_with(' ') && !t.is_empty() && !t.starts_with(['*', '-', '+']);
                if !continues {
                    break;
                }
                last = i;
            }
            let line = lines[last].trim_end();
            out[last] = match line.strip_suffix(':') {
                Some(head) => format!("{} {suffix}:", head.trim_end()),
                None => format!("{line} {suffix}"),
            };
        }
        None => out.insert(header + 1, format!("- References: {}", missing.join(", "))),
    }
    let mut text = out.join("\n");
    if changes.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

/// The body of the newest `.changes` entry, without its header.
pub fn newest_entry_body(changes: &str) -> Option<String> {
    crate::update_project::latest_changes_entry(changes)
}

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

// ---------------------------------------------------------------------------
// Mode
// ---------------------------------------------------------------------------

/// What a maintenance run does to each code stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keep each code stream's version and add the upstream fix as a patch:
    /// what maintenance normally accepts. The default of `update --maintenance`.
    Backport,
    /// Update each code stream to the version `update` finds.
    Update,
    /// `lint --maintenance`: packaging fixes, the version stays.
    Lint,
}

impl Mode {
    /// `--maintenance-mode`: `backport` (the default) or `update`. `lint`
    /// runs are always [`Mode::Lint`] and take no mode.
    pub fn parse(given: Option<&str>, lint: bool) -> Result<Mode, String> {
        match (given.map(|g| g.trim().to_lowercase()), lint) {
            (None, true) => Ok(Mode::Lint),
            (Some(_), true) => Err(
                "--maintenance-mode is for update; lint --maintenance always keeps the version"
                    .to_string(),
            ),
            (None, false) => Ok(Mode::Backport),
            (Some(m), false) => match m.as_str() {
                "backport" => Ok(Mode::Backport),
                "update" => Ok(Mode::Update),
                other => Err(format!(
                    "--maintenance-mode {other}: use backport (keep the version, add the fix) or update (a version update)"
                )),
            },
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Backport => "backport",
            Mode::Update => "update",
            Mode::Lint => "lint",
        }
    }
}

/// Whether the backport flags fit the mode, refs and packages of a run.
pub fn validate_mode(
    mode: Mode,
    backports: &[String],
    backport_repo: Option<&str>,
    no_bug_ref: bool,
    packages: &[String],
) -> Result<(), String> {
    match mode {
        Mode::Backport => {
            if backports.is_empty() {
                return Err(
                    "a maintenance backport needs the fix: pass --backport PATCH|URL|COMMIT (repeatable), \
                     or --maintenance-mode update for a version update"
                        .to_string(),
                );
            }
            if no_bug_ref {
                return Err(
                    "a backport fixes a tracked bug: --no-bug-ref is refused, pass --bug bsc#NNNNNNN \
                     (and --bug CVE-YYYY-NNNN)"
                        .to_string(),
                );
            }
            if packages.len() != 1 {
                return Err(format!(
                    "a backport is one fix of one package: --only names {}",
                    if packages.is_empty() {
                        "none".to_string()
                    } else {
                        packages.join(", ")
                    }
                ));
            }
        }
        Mode::Update | Mode::Lint => {
            if !backports.is_empty() || backport_repo.is_some() {
                return Err(format!(
                    "--backport and --backport-repo are for --maintenance-mode backport, not {}",
                    mode.name()
                ));
            }
        }
    }
    if backport_repo.is_some() && backports.is_empty() {
        return Err("--backport-repo names the repository of a --backport commit id".to_string());
    }
    Ok(())
}

/// The line that says what kind of update a request is, for the plan and the
/// request description.
pub fn mode_line(mode: Mode, job: Option<&backport::Job>) -> String {
    match (mode, job) {
        (Mode::Backport, Some(job)) => {
            let patches: Vec<String> = job
                .patches
                .iter()
                .map(|p| format!("{} ({})", p.file_name, p.origin))
                .collect();
            format!(
                "Backport of {} to the version each code stream has (no version update).",
                patches.join(", ")
            )
        }
        (Mode::Backport, None) => {
            "Backport to the version each code stream has (no version update).".to_string()
        }
        (Mode::Update, _) => "Version update of the package in each code stream.".to_string(),
        (Mode::Lint, _) => "Packaging fixes; each code stream keeps its version.".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Which code streams a fix affects
// ---------------------------------------------------------------------------

/// Whether a code stream needs the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Affected {
    Affected,
    /// Left out of the run, with why (shown in the plan and the summary).
    NotAffected(String),
    /// Nobody knows: the code stream is processed (no filtering).
    Unknown,
}

/// The fix a [`StreamCheck`] is asked about.
#[derive(Debug, Clone, Copy)]
pub struct FixQuery<'a> {
    /// The bug references (`bsc#..`, `CVE-..`).
    pub refs: &'a [String],
    /// The `--backport` sources as given (empty for an update).
    pub sources: &'a [String],
}

/// The extension point for the tool that knows which code streams a fix
/// affects. Given the package and the maintained code streams (update
/// projects), it returns one verdict per code stream, in the same order. The
/// run builds every code stream that is not [`Affected::NotAffected`].
pub trait StreamCheck {
    fn check(&self, package: &str, streams: &[String], fix: &FixQuery<'_>) -> Vec<Affected>;
}

/// The default: nothing is known, every code stream is processed.
pub struct NoStreamCheck;

impl StreamCheck for NoStreamCheck {
    fn check(&self, _package: &str, streams: &[String], _fix: &FixQuery<'_>) -> Vec<Affected> {
        vec![Affected::Unknown; streams.len()]
    }
}

/// `--affected-by COMMAND`: runs `COMMAND PACKAGE CODESTREAM REFS [SOURCE...]`
/// once per code stream, without a shell and with a timeout, where REFS is the
/// comma-separated bug references and SOURCE each `--backport` argument. The
/// first line of its stdout is `affected`, `not-affected <reason>` or
/// `unknown`; anything else, a failure or a timeout counts as `unknown`.
pub struct CommandCheck {
    pub program: String,
    pub timeout: Duration,
}

/// How long `--affected-by` may take per code stream.
pub const AFFECTED_BY_TIMEOUT: Duration = Duration::from_secs(60);

/// The arguments [`CommandCheck`] passes.
pub fn affected_by_args(package: &str, stream: &str, fix: &FixQuery<'_>) -> Vec<String> {
    let mut args = vec![package.to_string(), stream.to_string(), fix.refs.join(",")];
    args.extend(fix.sources.iter().cloned());
    args
}

/// The verdict in the output of an `--affected-by` command.
pub fn parse_affected(stdout: &str, success: bool) -> Affected {
    if !success {
        return Affected::Unknown;
    }
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    match word.to_lowercase().as_str() {
        "affected" if rest.trim().is_empty() => Affected::Affected,
        "not-affected" => {
            let reason: String = rest
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            Affected::NotAffected(if reason.is_empty() {
                "the --affected-by check says it is not affected".to_string()
            } else {
                reason
            })
        }
        _ => Affected::Unknown,
    }
}

impl StreamCheck for CommandCheck {
    fn check(&self, package: &str, streams: &[String], fix: &FixQuery<'_>) -> Vec<Affected> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        streams
            .iter()
            .map(|s| {
                match proc::capture_with_timeout(
                    &self.program,
                    &affected_by_args(package, s, fix),
                    &cwd,
                    Emit::quiet(),
                    self.timeout,
                ) {
                    Ok(out) => parse_affected(&out.stdout, out.success()),
                    Err(_) => Affected::Unknown,
                }
            })
            .collect()
    }
}

/// The verdict of `check` for each of `streams`; a check that returns too few
/// leaves the rest [`Affected::Unknown`].
pub fn verdicts(
    check: &dyn StreamCheck,
    package: &str,
    streams: &[String],
    fix: &FixQuery<'_>,
) -> Vec<(String, Affected)> {
    let mut got = check.check(package, streams, fix).into_iter();
    streams
        .iter()
        .map(|s| (s.clone(), got.next().unwrap_or(Affected::Unknown)))
        .collect()
}

// ---------------------------------------------------------------------------
// Per code stream results
// ---------------------------------------------------------------------------

/// How one code stream of a package ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamOutcome {
    /// In the request; how its change went (`applied cleanly`, `updated to 1.2`).
    Ready(String),
    /// Not in the request, for a reason that is no failure (already submitted,
    /// nothing changed, declined).
    LeftOut(String),
    /// Its build or its backport failed; never sent.
    Failed(String),
    /// The `--affected-by` check says the fix is not needed there.
    NotAffected(String),
    /// Maintained in git (SLFO); not supported yet.
    GitMaintained,
}

/// One code stream's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamReport {
    /// The update project, or the `[git] ORG/PKG:BRANCH` stream.
    pub stream: String,
    pub outcome: StreamOutcome,
}

/// The summary of a package's code streams, one line each.
pub fn summary_lines(pkg: &str, reports: &[StreamReport]) -> Vec<String> {
    let ready = reports
        .iter()
        .filter(|r| matches!(r.outcome, StreamOutcome::Ready(_)))
        .count();
    let mut out = vec![format!(
        "{pkg}: {ready} of {} code stream{} ready for the maintenance request:",
        reports.len(),
        if reports.len() == 1 { "" } else { "s" }
    )];
    for r in reports {
        let what = match &r.outcome {
            StreamOutcome::Ready(n) => format!("ready, {n}"),
            StreamOutcome::LeftOut(w) => format!("left out, {w}"),
            StreamOutcome::Failed(w) => format!("FAILED, {w}"),
            StreamOutcome::NotAffected(w) => format!("not affected, {w}"),
            StreamOutcome::GitMaintained => {
                "skipped: maintained in git (SLFO), which is not supported yet".to_string()
            }
        };
        out.push(format!("  {}: {what}", r.stream));
    }
    out
}

/// The code streams of `reports` that are not in the request, with why, for
/// [`plan_lines`].
pub fn left_out(reports: &[StreamReport]) -> Vec<(String, String)> {
    reports
        .iter()
        .filter_map(|r| {
            let why = match &r.outcome {
                StreamOutcome::Ready(_) => return None,
                StreamOutcome::LeftOut(w) => w.clone(),
                StreamOutcome::Failed(w) => format!("failed: {w}"),
                StreamOutcome::NotAffected(w) => format!("not affected: {w}"),
                StreamOutcome::GitMaintained => {
                    "maintained in git (SLFO), not supported yet".to_string()
                }
            };
            Some((r.stream.clone(), why))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Build paths
// ---------------------------------------------------------------------------

/// Why a maintenance run does not add the build paths `paths` (`PROJECT/REPO`)
/// the dependency resolver chose for `pkg`, or `None` when it needs none. The
/// incident builds against the code stream only, so a package that needs
/// another project cannot be built for it as it is; the code stream fails
/// instead of building something the incident cannot reproduce.
pub fn build_path_refusal(pkg: &str, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    Some(format!(
        "{pkg} needs the build path{} {} to build, which a maintenance incident does not have: \
         not added; the code stream needs the dependency itself (a maintenance update of it first)",
        if paths.len() == 1 { "" } else { "s" },
        paths.join(", ")
    ))
}

/// The reason the dependency resolver of a maintenance run gives for not
/// adding a build path (see `DepResolver::with_frozen_paths`).
pub const FROZEN_PATHS_REASON: &str =
    "a maintenance run adds no build paths: the incident builds against the code stream only";

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// What `update --maintenance` / `lint --maintenance` was asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// The packages (`--only`); maintenance is per package.
    pub packages: Vec<String>,
    /// The code streams named; empty means: ask which.
    pub codestreams: Vec<String>,
    /// `--branch-project`, for a single code stream.
    pub branch_project: Option<String>,
    pub bugs: Vec<String>,
    pub no_bug_ref: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub distro: Option<String>,
    pub arch: String,
    /// Where the checkouts that add the bug references go.
    pub checkout_dir: PathBuf,
    /// The workspace state directory (`state/`), where a build recorded the
    /// risk flags `--yes` must not submit past.
    pub state_root: PathBuf,
    /// Backport (the default of `update`), update or lint.
    pub mode: Mode,
    /// `--backport`: the fixes of a backport, as given.
    pub backports: Vec<String>,
    /// `--backport-repo`: the repository of a `--backport` commit id.
    pub backport_repo: Option<String>,
    /// `--affected-by`: the command that tells which code streams need the
    /// fix (see [`CommandCheck`]); without it every code stream is processed.
    pub affected_by: Option<String>,
}

/// One code stream's build, as the run asks the command line for it.
pub struct BuildJob<'a> {
    /// The project the ordinary run browses.
    pub source: &'a str,
    /// The branch project it builds in.
    pub branch: &'a str,
    /// The update project.
    pub release: &'a str,
    pub mode: Mode,
    /// What to put into the branch checkout and ask of the agent (backport).
    pub backport: Option<&'a Arc<backport::Job>>,
}

/// What the run needs from the command line.
pub struct Host<'a> {
    pub say: &'a dyn Fn(&str),
    pub warn: &'a dyn Fn(&str),
    /// A line typed on the terminal after showing the question; `None` when
    /// there is no terminal.
    pub ask: &'a dyn Fn(&str) -> Option<String>,
    /// A yes/no on the terminal; no without one.
    pub confirm: &'a dyn Fn(&str) -> bool,
    /// Run the ordinary `update`/`lint` for one code stream (a dry run when
    /// the run is one): in update mode as `update` does, in backport mode at
    /// the code stream's own version with the backports staged.
    pub build: &'a mut dyn FnMut(&BuildJob<'_>) -> Result<(), String>,
}

/// One code stream picked for the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The update project.
    pub project: String,
    /// The project the ordinary run browses.
    pub source: String,
    /// Where the change is branched and built.
    pub branch: String,
}

/// `--maintenance-list`: print where each package is maintained. Read only.
pub fn list(packages: &[String], host: &Host<'_>) -> Result<(), String> {
    if packages.is_empty() {
        return Err("--maintenance-list needs the packages: pass --only PKG[,PKG...]".to_string());
    }
    for pkg in packages {
        let found = maintained_codestreams(pkg, Emit::quiet()).map_err(|e| e.to_string())?;
        let git = git_maintained(pkg, Emit::quiet()).unwrap_or_default();
        if found.is_empty() && git.is_empty() {
            (host.say)(&format!("{pkg}: not maintained in any code stream"));
            continue;
        }
        (host.say)(&format!("{pkg} is maintained in:"));
        for c in found {
            let devel = c
                .devel
                .map(|(p, n)| format!(" (using sources from {p}/{n})"))
                .unwrap_or_default();
            (host.say)(&format!("  {}/{}{devel}", c.project, c.package));
        }
        for g in git {
            (host.say)(&format!("  {g} (maintained in git: not supported yet)"));
        }
    }
    Ok(())
}

/// The warning for a code stream that takes its sources from `devel`: a
/// running incident, or another code stream the fix normally goes to.
pub fn devel_warning(project: &str, pkg: &str, devel: &(String, String)) -> String {
    let (p, n) = devel;
    if p.contains(":Maintenance:") {
        format!(
            "{project}: an incident ({p}/{n}) is already running for {pkg}; the maintenance team \
             may merge the new request into it"
        )
    } else {
        format!(
            "{project}: {pkg} takes its sources from {p}/{n}; a fix normally goes to that code stream"
        )
    }
}

/// The code streams the run goes to: the named ones, refused unless every
/// package is maintained there, or the ones the user picks.
fn choose(
    opts: &Options,
    per_package: &[Vec<Codestream>],
    host: &Host<'_>,
) -> Result<Vec<String>, String> {
    for (pkg, found) in opts.packages.iter().zip(per_package) {
        if found.is_empty() {
            return Err(format!(
                "{pkg} is not maintained in any code stream on OBS; there is nothing to send a maintenance request to"
            ));
        }
        for c in found {
            if let Some(d) = &c.devel {
                (host.warn)(&devel_warning(&c.project, pkg, d));
            }
        }
    }
    if !opts.codestreams.is_empty() {
        let mut chosen: Vec<String> = Vec::new();
        for (pkg, found) in opts.packages.iter().zip(per_package) {
            let picked =
                select_codestreams(found, &opts.codestreams).map_err(|e| format!("{pkg}: {e}"))?;
            for c in picked {
                if !chosen.contains(&c.project) {
                    chosen.push(c.project);
                }
            }
        }
        // `all` per package may differ; keep only what every package has.
        let common = common_codestreams(per_package);
        if let Some(odd) = chosen.iter().find(|c| !common.contains(c)) {
            return Err(format!(
                "{odd} does not maintain every package of --only; run them separately"
            ));
        }
        return Ok(chosen);
    }
    let common = common_codestreams(per_package);
    if common.is_empty() {
        return Err("the packages of --only share no code stream; run them separately".to_string());
    }
    (host.say)("code streams that maintain the package:");
    for l in pick_lines(&common) {
        (host.say)(&l);
    }
    let answer = (host.ask)("Send the change to which code streams? (numbers or names, comma separated, or all) ")
        .ok_or("cannot ask which code streams without a terminal: pass --maintenance CODESTREAM[,CODESTREAM...]")?;
    parse_pick(&answer, &common)
}

/// The newest `.changes` entry of `project/pkg`, with the file name and text.
fn read_changes(project: &str, pkg: &str, emit: Emit<'_>) -> Option<(String, String)> {
    let files = osc::list_package_files(project, pkg, emit)?;
    if files.is_empty() {
        let incident = newest_incident_package(project, pkg, emit)?;
        let (name, text) = read_changes(project, &incident, emit)?;
        return Some((name, text));
    }
    let own = format!("{pkg}.changes");
    let name = files
        .iter()
        .find(|f| **f == own)
        .or_else(|| files.iter().find(|f| f.ends_with(".changes")))?;
    Some((name.clone(), osc::cat_file(project, pkg, name, emit)?))
}

/// The version a code stream's package must still have after a backport or
/// lint run (`release`) against the branch's (`branch`), or why not.
pub fn version_kept(release: Option<&str>, branch: Option<&str>) -> Result<String, String> {
    match (release, branch) {
        (Some(r), Some(b)) if r == b => Ok(b.to_string()),
        (Some(r), Some(b)) => Err(format!(
            "the version changed from {r} to {b}; a backport keeps the code stream's version"
        )),
        (None, _) => Err("could not read the code stream's version".to_string()),
        (_, None) => Err("could not read the version of the branch".to_string()),
    }
}

/// The changelog bullets of a backport: `(file, bullet)` per patch.
pub fn backport_bullets(job: Option<&backport::Job>, refs: &[String]) -> Vec<(String, String)> {
    job.map(|j| {
        j.patches
            .iter()
            .map(|p| (p.file_name.clone(), backport::changes_bullet(p, refs)))
            .collect()
    })
    .unwrap_or_default()
}

/// `changes` with the backport bullets and the missing references in its
/// newest entry; `None` when it has them all.
pub fn amend_changes(
    changes: &str,
    refs: &[String],
    bullets: &[(String, String)],
) -> Option<String> {
    let with_bullets = backport::with_backport_bullets(changes, bullets);
    let base = with_bullets.as_deref().unwrap_or(changes);
    with_refs_in_newest_entry(base, refs).or(with_bullets)
}

/// One code stream of a package that may get a request, with what it takes.
struct Ready {
    action: IncidentAction,
    /// The `.changes` file name and its text with the bullets and references
    /// added, when they are missing from the branch.
    amended: Option<(String, String)>,
    entry: Option<String>,
    /// How its change went, for the plan and the summary.
    note: String,
}

fn spec_version(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    crate::update::spec_for_package(project, pkg, emit)
        .ok()
        .as_deref()
        .and_then(crate::spec::parse_header)
        .map(|h| h.version)
}

/// Whether `pkg` in the branch of `t` may go into a request; how the stream
/// ended otherwise.
fn check_stream(
    t: &Target,
    pkg: &str,
    refs: &[String],
    opts: &Options,
    job: Option<&backport::Job>,
) -> Result<Ready, StreamOutcome> {
    let emit = Emit::quiet();
    let repo = osc::choose_repository(&t.branch, opts.distro.as_deref(), &opts.arch).0;
    match osc::get_overall_build_status(&t.branch, pkg, &repo, &opts.arch) {
        Ok((osc::BuildStatus::Succeeded, _)) => {}
        Ok((status, at)) => {
            let at = at.map(|a| format!(" on {a}")).unwrap_or_default();
            return Err(StreamOutcome::Failed(format!(
                "its build in {} is {status}{at}, not succeeded",
                t.branch
            )));
        }
        Err(e) => {
            return Err(StreamOutcome::Failed(format!(
                "could not read its build status in {}: {e}",
                t.branch
            )));
        }
    }
    let Some((name, text)) = read_changes(&t.branch, pkg, emit) else {
        return Err(StreamOutcome::Failed(format!(
            "could not read its .changes in {}",
            t.branch
        )));
    };
    let entry = newest_entry_body(&text);
    let origin_entry = read_changes(&t.project, pkg, emit).and_then(|(_, o)| newest_entry_body(&o));
    if entry.is_some() && entry == origin_entry {
        return Err(StreamOutcome::LeftOut(format!(
            "{} has no change of its own over {}",
            t.branch, t.project
        )));
    }
    let branch_spec = crate::update::spec_for_package(&t.branch, pkg, emit).ok();
    let branch_version = branch_spec
        .as_deref()
        .and_then(crate::spec::parse_header)
        .map(|h| h.version);
    let note = match opts.mode {
        Mode::Update => format!(
            "updated to {}",
            branch_version.as_deref().unwrap_or("an unread version")
        ),
        Mode::Backport | Mode::Lint => {
            let release_version =
                spec_version(&t.project, pkg, emit).or_else(|| spec_version(&t.source, pkg, emit));
            let version = version_kept(release_version.as_deref(), branch_version.as_deref())
                .map_err(StreamOutcome::Failed)?;
            match job {
                Some(job) => {
                    let spec = branch_spec.unwrap_or_default();
                    let mut notes = Vec::new();
                    for p in &job.patches {
                        let committed = osc::cat_file(&t.branch, pkg, &p.file_name, emit);
                        match backport::classify(p, committed.as_deref(), &spec) {
                            backport::Applied::Missing(why) => {
                                return Err(StreamOutcome::Failed(format!(
                                    "the backport did not go in: {why}"
                                )));
                            }
                            applied => notes.push(format!("{} {applied}", p.file_name)),
                        }
                    }
                    format!("{version}: {}", notes.join("; "))
                }
                None => format!("packaging fixes at {version}"),
            }
        }
    };
    match open_incident(&t.branch, pkg, &t.project, emit) {
        Ok(Some(why)) => return Err(StreamOutcome::LeftOut(why)),
        Ok(None) => {}
        Err(e) => {
            return Err(StreamOutcome::Failed(format!(
                "could not check for an earlier request: {e}"
            )));
        }
    }
    let amended = amend_changes(&text, refs, &backport_bullets(job, refs));
    let entry = amended.as_deref().map_or(entry, newest_entry_body);
    Ok(Ready {
        action: IncidentAction {
            source_project: t.branch.clone(),
            source_package: pkg.to_string(),
            release_project: t.project.clone(),
        },
        amended: amended.map(|a| (name, a)),
        entry,
        note,
    })
}

/// Commit the backport bullets and the references into the branch's `.changes`.
/// Returns the baseline kept for the commit (see [`undo_refs`]), `None` when
/// nothing was committed. A commit that fails is undone on OBS here.
fn commit_refs(
    branch: &str,
    pkg: &str,
    name: &str,
    opts: &Options,
    refs: &[String],
    bullets: &[(String, String)],
) -> Result<Option<PathBuf>, String> {
    let emit = Emit::quiet();
    let dir = osc::checkout(branch, pkg, &opts.checkout_dir, emit).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let Some(amended) = amend_changes(&text, refs, bullets) else {
        return Ok(None);
    };
    let baseline = osc::begin_baseline(
        &opts.checkout_dir.join(osc::BASELINE_ROOT),
        &dir,
        branch,
        pkg,
        emit,
    )
    .map_err(|e| format!("cannot keep the OBS baseline of {branch}: {e}"))?;
    std::fs::write(&path, amended).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    osc::note_attempt(&baseline, &[name.to_string()]).map_err(|e| e.to_string())?;
    let committed = osc::commit(
        &dir,
        &format!(
            "Add the bug references {} (auto-package maintenance)",
            refs.join(", ")
        ),
        &[name.to_string()],
        emit,
    );
    match committed {
        Ok(()) => Ok(Some(baseline)),
        Err(e) => match osc::restore_baseline(&baseline, emit) {
            Ok(()) => Err(e.to_string()),
            Err(why) => Err(format!("{e}; the OBS baseline was NOT restored: {why}")),
        },
    }
}

/// The risk lines `--yes` must not submit past for `pkg`: what its recorded
/// state says needs a review, or why that state cannot be read. A package with
/// no recorded state was never built here, so it has none.
fn yes_blockers(state_root: &std::path::Path, pkg: &str) -> Vec<String> {
    match crate::risk_flags::try_of_package(state_root, pkg) {
        Ok(flags) => flags
            .iter()
            .map(crate::risk_flags::RiskFlag::line)
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => vec![format!("its recorded state cannot be read: {e}")],
    }
}

/// Put back the OBS baselines of branches whose references were committed, for
/// a maintenance request that could not be created: the references must not
/// stay on the branches without their request. Returns what could not go back.
fn undo_refs(bases: &[PathBuf]) -> Option<String> {
    let failed: Vec<String> = bases
        .iter()
        .filter_map(|b| osc::restore_baseline(b, Emit::quiet()).err())
        .collect();
    (!failed.is_empty()).then(|| failed.join("; "))
}

/// The [`StreamCheck`] of a run: `--affected-by` when given, else none.
pub fn stream_check_for(opts: &Options) -> Box<dyn StreamCheck> {
    match &opts.affected_by {
        Some(program) => Box::new(CommandCheck {
            program: program.clone(),
            timeout: AFFECTED_BY_TIMEOUT,
        }),
        None => Box::new(NoStreamCheck),
    }
}

/// `update --maintenance` / `lint --maintenance` with the [`StreamCheck`] of
/// the options (see [`run_with`]).
pub fn run(opts: &Options, host: &mut Host<'_>) -> Result<(), String> {
    let check = stream_check_for(opts);
    run_with(opts, host, check.as_ref())
}

/// The streams of `chosen` that every package's verdict leaves out.
pub fn streams_to_build(chosen: &[String], per_package: &[Vec<(String, Affected)>]) -> Vec<String> {
    chosen
        .iter()
        .filter(|s| {
            !per_package.iter().all(|v| {
                v.iter()
                    .any(|(cs, a)| cs == *s && matches!(a, Affected::NotAffected(_)))
            })
        })
        .cloned()
        .collect()
}

/// Build the change in a branch of every chosen code stream that `check` does
/// not rule out, each from its own package, and send the packages that built
/// to the maintenance project, after showing the plan and asking (unless
/// `yes`). A dry run builds nothing, asks nothing and changes nothing on OBS.
/// Every code stream ends in a [`StreamReport`], printed per package.
pub fn run_with(
    opts: &Options,
    host: &mut Host<'_>,
    check: &dyn StreamCheck,
) -> Result<(), String> {
    if opts.packages.is_empty() {
        return Err("a maintenance request is per package: pass --only PKG[,PKG...]".to_string());
    }
    validate_mode(
        opts.mode,
        &opts.backports,
        opts.backport_repo.as_deref(),
        opts.no_bug_ref,
        &opts.packages,
    )?;
    let refs = parse_bug_refs(&opts.bugs, opts.no_bug_ref)?;
    for w in bug_ref_warnings(&refs) {
        (host.warn)(&w);
    }
    let job: Option<Arc<backport::Job>> = if opts.mode == Mode::Backport {
        let job = backport::prepare(&opts.backports, opts.backport_repo.as_deref(), &refs)?;
        for p in &job.patches {
            (host.say)(&format!(
                "backport {}: {} (from {})",
                p.file_name, p.reason, p.origin
            ));
        }
        Some(Arc::new(job))
    } else {
        None
    };
    let mode_text = mode_line(opts.mode, job.as_deref());
    let emit = Emit::quiet();
    let mut per_package = Vec::new();
    let mut git: Vec<Vec<GitStream>> = Vec::new();
    for pkg in &opts.packages {
        per_package.push(maintained_codestreams(pkg, emit).map_err(|e| format!("{pkg}: {e}"))?);
        git.push(git_maintained(pkg, emit).unwrap_or_else(|e| {
            (host.warn)(&format!(
                "{pkg}: could not check for code streams maintained in git: {e}"
            ));
            Vec::new()
        }));
    }
    for (pkg, g) in opts.packages.iter().zip(&git) {
        for s in g {
            (host.say)(&format!(
                "{pkg}: {s} is maintained in git (SLFO), which is not supported yet: left out"
            ));
        }
    }
    let chosen = choose(opts, &per_package, host)?;
    if opts.branch_project.is_some() && chosen.len() > 1 {
        return Err(
            "--branch-project names one project, but each code stream is built in its own: drop it, or pick one code stream"
                .to_string(),
        );
    }

    // Which code streams need the fix (the --affected-by seam).
    let fix = FixQuery {
        refs: &refs,
        sources: &opts.backports,
    };
    let affected: Vec<Vec<(String, Affected)>> = opts
        .packages
        .iter()
        .map(|pkg| verdicts(check, pkg, &chosen, &fix))
        .collect();
    let mut reports: Vec<Vec<StreamReport>> = affected
        .iter()
        .map(|v| {
            v.iter()
                .filter_map(|(s, a)| match a {
                    Affected::NotAffected(why) => Some(StreamReport {
                        stream: s.clone(),
                        outcome: StreamOutcome::NotAffected(why.clone()),
                    }),
                    _ => None,
                })
                .collect()
        })
        .collect();
    for (r, g) in reports.iter_mut().zip(&git) {
        r.extend(g.iter().map(|s| StreamReport {
            stream: s.to_string(),
            outcome: StreamOutcome::GitMaintained,
        }));
    }
    let chosen = streams_to_build(&chosen, &affected);
    let print_summaries = |reports: &[Vec<StreamReport>]| {
        for (pkg, r) in opts.packages.iter().zip(reports) {
            for l in summary_lines(pkg, r) {
                (host.say)(&l);
            }
        }
    };
    if chosen.is_empty() {
        (host.say)("no code stream is left to build: the fix affects none of them");
        print_summaries(&reports);
        return Ok(());
    }

    let user = osc::current_user(emit).ok();
    let target_project = maintenance_project(emit).map_err(|e| e.to_string())?;

    let mut targets: Vec<Target> = Vec::new();
    for project in &chosen {
        let branch = match (&opts.branch_project, &user) {
            (Some(b), _) => b.clone(),
            (None, Some(u)) => branch_project_for(u, project),
            (None, None) => {
                return Err(
                    "could not read the OBS user to name the branch project; pass --branch-project"
                        .to_string(),
                );
            }
        };
        let listed = osc::try_list_project_packages(project, emit)?;
        let source = if opts.packages.iter().all(|p| listed.contains(p)) {
            project.clone()
        } else {
            let meta = osc::get_project_meta(project, emit)
                .map_err(|e| format!("could not read the meta of {project}: {e}"))?
                .unwrap_or_default();
            match planning_source(project, false, &meta) {
                Some(s) => {
                    (host.say)(&format!(
                        "{project} has no update of the package yet; browsing {s}, which it links to"
                    ));
                    s
                }
                None => {
                    return Err(format!(
                        "{project} does not have the package and links to no project that does"
                    ));
                }
            }
        };
        targets.push(Target {
            project: project.clone(),
            source,
            branch,
        });
    }

    // Each code stream is built from its own package; a failed one is noted
    // and the others go on.
    let mut build_failed: Vec<(String, String)> = Vec::new();
    for t in &targets {
        (host.say)(&format!(
            "code stream {} ({}): building in {} from {}",
            t.project,
            opts.mode.name(),
            t.branch,
            t.source
        ));
        let bj = BuildJob {
            source: &t.source,
            branch: &t.branch,
            release: &t.project,
            mode: opts.mode,
            backport: job.as_ref(),
        };
        if let Err(e) = (host.build)(&bj) {
            (host.warn)(&format!("{}: {e}", t.project));
            build_failed.push((t.project.clone(), e));
        }
    }
    // With one package the build's error is that package's; with several, its
    // state on OBS decides per package below.
    let failed_build = |t: &Target| -> Option<String> {
        if opts.packages.len() != 1 {
            return None;
        }
        build_failed
            .iter()
            .find(|(p, _)| *p == t.project)
            .map(|(_, e)| e.clone())
    };
    let bullets = backport_bullets(job.as_deref(), &refs);

    if opts.dry_run {
        (host.say)(&format!(
            "dry run: no maintenance request is created; each package that builds would go to {target_project}:"
        ));
        for (i, pkg) in opts.packages.iter().enumerate() {
            let mut ok: Vec<&Target> = Vec::new();
            for t in &targets {
                match failed_build(t) {
                    Some(e) => reports[i].push(StreamReport {
                        stream: t.project.clone(),
                        outcome: StreamOutcome::Failed(e),
                    }),
                    None => {
                        reports[i].push(StreamReport {
                            stream: t.project.clone(),
                            outcome: StreamOutcome::Ready(format!(
                                "would be built ({})",
                                opts.mode.name()
                            )),
                        });
                        ok.push(t);
                    }
                }
            }
            let entry = if bullets.is_empty() {
                format!("<the newest .changes entry, with {}>", refs_suffix(&refs))
            } else {
                bullets
                    .iter()
                    .map(|(_, b)| b.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let req = MaintenanceRequest {
                target_project: target_project.clone(),
                actions: ok
                    .iter()
                    .map(|t| IncidentAction {
                        source_project: t.branch.clone(),
                        source_package: pkg.clone(),
                        release_project: t.project.clone(),
                    })
                    .collect(),
                sourceupdate: ok
                    .iter()
                    .all(|t| sourceupdate_for(&t.branch, user.as_deref()).is_some())
                    .then(|| "cleanup".to_string()),
                description: request_description(pkg, &mode_text, &[("".into(), Some(entry))], &[]),
            };
            for l in plan_lines(&req, &refs, &mode_text, &[], &left_out(&reports[i])) {
                (host.say)(&l);
            }
        }
        print_summaries(&reports);
        return Ok(());
    }

    let mut failures: Vec<String> = Vec::new();
    for (i, pkg) in opts.packages.iter().enumerate() {
        let mut ready: Vec<Ready> = Vec::new();
        for t in &targets {
            let outcome = match failed_build(t) {
                Some(e) => Err(StreamOutcome::Failed(e)),
                None => check_stream(t, pkg, &refs, opts, job.as_deref()),
            };
            match outcome {
                Ok(r) => ready.push(r),
                Err(o) => {
                    (host.say)(&format!(
                        "no maintenance request for {pkg} in {}: {}",
                        t.project,
                        match &o {
                            StreamOutcome::Failed(w)
                            | StreamOutcome::LeftOut(w)
                            | StreamOutcome::NotAffected(w)
                            | StreamOutcome::Ready(w) => w.as_str(),
                            StreamOutcome::GitMaintained => "maintained in git",
                        }
                    ));
                    reports[i].push(StreamReport {
                        stream: t.project.clone(),
                        outcome: o,
                    });
                }
            }
        }
        if ready.is_empty() {
            continue;
        }
        let entries: Vec<(String, Option<String>)> = ready
            .iter()
            .map(|r| (r.action.release_project.clone(), r.entry.clone()))
            .collect();
        let notes: Vec<(String, String)> = ready
            .iter()
            .map(|r| (r.action.release_project.clone(), r.note.clone()))
            .collect();
        let sourceupdate = ready
            .iter()
            .all(|r| sourceupdate_for(&r.action.source_project, user.as_deref()).is_some())
            .then(|| "cleanup".to_string());
        let req = MaintenanceRequest {
            target_project: target_project.clone(),
            actions: ready.iter().map(|r| r.action.clone()).collect(),
            sourceupdate,
            description: request_description(pkg, &mode_text, &entries, &refs),
        };
        for l in plan_lines(&req, &refs, &mode_text, &notes, &left_out(&reports[i])) {
            (host.say)(&l);
        }
        for r in &ready {
            if r.amended.is_some() {
                (host.say)(&format!(
                    "  the changelog bullets and references are added to the newest entry of {}/{} first",
                    r.action.source_project, pkg
                ));
            }
        }
        // `--yes` approves without asking, so a package whose change needs a
        // review (or whose recorded state cannot be read) gets no request.
        let blockers = if opts.yes {
            yes_blockers(&opts.state_root, pkg)
        } else {
            Vec::new()
        };
        if !blockers.is_empty() {
            (host.warn)(&format!(
                "--yes refuses to create the maintenance request for {pkg}: it needs a review; \
                 run without --yes to review it"
            ));
            for line in &blockers {
                (host.warn)(&format!("  {line}"));
            }
            for r in &ready {
                reports[i].push(StreamReport {
                    stream: r.action.release_project.clone(),
                    outcome: StreamOutcome::Failed(
                        "--yes refuses to submit a change that needs a review".into(),
                    ),
                });
            }
            continue;
        }
        if !opts.yes && !(host.confirm)(&format!("Create the maintenance request for {pkg}?")) {
            (host.say)(&format!("{pkg}: no maintenance request (declined)"));
            for r in &ready {
                reports[i].push(StreamReport {
                    stream: r.action.release_project.clone(),
                    outcome: StreamOutcome::LeftOut("the request was declined".into()),
                });
            }
            continue;
        }
        // A code stream whose changelog cannot be amended is left out; the
        // others still go.
        let mut sent: Vec<Ready> = Vec::new();
        let mut bases: Vec<PathBuf> = Vec::new();
        for r in ready {
            if let Some((name, _)) = &r.amended {
                match commit_refs(&r.action.source_project, pkg, name, opts, &refs, &bullets) {
                    Ok(base) => bases.extend(base),
                    Err(e) => {
                        reports[i].push(StreamReport {
                            stream: r.action.release_project.clone(),
                            outcome: StreamOutcome::Failed(format!(
                                "could not add the changelog bullets and references in {}: {e}",
                                r.action.source_project
                            )),
                        });
                        continue;
                    }
                }
            }
            sent.push(r);
        }
        if sent.is_empty() {
            continue;
        }
        let req = MaintenanceRequest {
            actions: sent.iter().map(|r| r.action.clone()).collect(),
            ..req
        };
        match create_request(&req, emit) {
            Ok(id) => {
                for base in &bases {
                    let _ = std::fs::remove_dir_all(base);
                }
                (host.say)(&format!(
                    "created maintenance request {id} for {pkg} to {}",
                    req.target_project
                ));
                for r in &sent {
                    reports[i].push(StreamReport {
                        stream: r.action.release_project.clone(),
                        outcome: StreamOutcome::Ready(format!("{} (request {id})", r.note)),
                    });
                }
            }
            Err(e) => {
                let not_restored = undo_refs(&bases).map_or(String::new(), |why| {
                    format!("; the OBS baseline was NOT restored: {why}")
                });
                for r in &sent {
                    reports[i].push(StreamReport {
                        stream: r.action.release_project.clone(),
                        outcome: StreamOutcome::Failed(format!(
                            "the maintenance request could not be created: {e}{not_restored}"
                        )),
                    });
                }
            }
        }
    }
    print_summaries(&reports);
    for (pkg, r) in opts.packages.iter().zip(&reports) {
        for rep in r {
            if let StreamOutcome::Failed(why) = &rep.outcome {
                failures.push(format!("{pkg} in {}: {why}", rep.stream));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod undo_refs_tests {
    use super::*;

    #[test]
    fn nothing_to_undo_is_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(undo_refs(&[]), None);
        assert_eq!(undo_refs(&[dir.path().join("never-written")]), None);
    }

    #[test]
    fn a_baseline_that_cannot_go_back_is_named() {
        // An attempt was noted, but the checkout it was saved from is gone.
        let dir = tempfile::tempdir().unwrap();
        osc::note_attempt(dir.path(), &["foo.changes".to_string()]).unwrap();
        let why = undo_refs(&[dir.path().to_path_buf()]).unwrap();
        assert!(
            why.contains("checkout the baseline was saved from"),
            "{why}"
        );
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn yes_blocks_a_package_whose_recorded_risks_need_a_review_or_cannot_be_read() {
        use crate::pipeline::PkgState;
        use crate::risk_flags::{RiskFlag, settle};
        use crate::workspace::Workspace;
        let root = tempfile::tempdir().unwrap();
        // Never built here: nothing recorded, nothing to block on.
        assert!(super::yes_blockers(root.path(), "python-new").is_empty());
        let ws = Workspace::open(root.path(), "python-foo").unwrap();
        PkgState::update(&ws, |st| {
            settle(
                st,
                "r1",
                vec![RiskFlag::NewScriptlet {
                    header: "%post".into(),
                }],
            )
        })
        .unwrap();
        assert_eq!(
            super::yes_blockers(root.path(), "python-foo"),
            vec!["RISK: new scriptlet: %post".to_string()]
        );
        std::fs::write(ws.dir().join("state.json"), "{ not json").unwrap();
        let lines = super::yes_blockers(root.path(), "python-foo");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("cannot be read"), "{lines:?}");
    }

    use super::*;

    fn cs(project: &str) -> Codestream {
        Codestream {
            project: project.into(),
            package: "curl".into(),
            devel: None,
        }
    }

    #[test]
    fn maintained_query_is_the_dry_run_osc_maintained_makes() {
        assert_eq!(
            maintained_api_path("libsigc++"),
            "/source?cmd=branch&attribute=OBS%3AMaintained&update_project_attribute=OBS%3AUpdateProject&dryrun=1&package=libsigc%2B%2B"
        );
    }

    #[test]
    fn maintained_collection_is_parsed_with_devel_and_incident_names() {
        let xml = r#"<collection>
  <package project="openSUSE:Backports:SLE-15-SP7:Update" package="Botan">
    <target project="home:u:branches:OBS_Maintained:Botan" package="Botan.openSUSE:Backports:SLE-15-SP7:Update"/>
  </package>
  <package project="SUSE:SLE-15-SP6:Update" package="Botan.36123"/>
  <package project="openSUSE:Leap:15.6:Update" package="Botan">
    <devel project="SUSE:SLE-15-SP6:Update" package="Botan"/>
  </package>
</collection>"#;
        let got = parse_maintained(xml);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].project, "openSUSE:Backports:SLE-15-SP7:Update");
        assert_eq!(got[0].package, "Botan");
        assert_eq!(got[0].devel, None);
        assert_eq!(got[1].package, "Botan");
        assert_eq!(
            got[2].devel,
            Some(("SUSE:SLE-15-SP6:Update".into(), "Botan".into()))
        );
        assert!(parse_maintained("<collection/>").is_empty());
    }

    #[test]
    fn osc_maintained_text_is_parsed_and_git_lines_skipped() {
        let text = "openSUSE:Backports:SLE-15-SP7:Update/Botan\n\
                    openSUSE:Leap:15.6:Update/curl using sources from SUSE:SLE-15-SP6:Update/curl\n\
                    [git] pool/curl:leap-16.0\n";
        let got = parse_maintained_text(text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].project, "openSUSE:Leap:15.6:Update");
        assert_eq!(
            got[1].devel,
            Some(("SUSE:SLE-15-SP6:Update".into(), "curl".into()))
        );
        assert!(says_nothing_maintained(
            "BuildService API error: no packages found by search criteria"
        ));
    }

    #[test]
    fn maintenance_project_comes_from_the_attribute_search() {
        assert_eq!(
            maintenance_project_search_path(),
            "/search/project/id?match=attribute%2F%40name%3D%27OBS%3AMaintenanceProject%27"
        );
        let xml =
            "<collection matches=\"1\">\n  <project name='openSUSE:Maintenance'/>\n</collection>";
        assert_eq!(
            parse_first_project(xml).as_deref(),
            Some("openSUSE:Maintenance")
        );
        assert_eq!(parse_first_project("<collection matches=\"0\"/>"), None);
    }

    #[test]
    fn codestreams_are_picked_by_name_short_name_or_all_and_others_refused() {
        let available = vec![cs("SUSE:SLE-15-SP7:Update"), cs("SUSE:SLE-15-SP6:Update")];
        let got = select_codestreams(&available, &["SUSE:SLE-15-SP6".into()]).unwrap();
        assert_eq!(got, vec![cs("SUSE:SLE-15-SP6:Update")]);
        assert_eq!(
            select_codestreams(&available, &["all".into()]).unwrap(),
            available
        );
        let err = select_codestreams(&available, &["SUSE:SLE-12-SP5:Update".into()]).unwrap_err();
        assert!(
            err.contains("SUSE:SLE-12-SP5:Update does not maintain"),
            "{err}"
        );
        assert!(
            err.contains("SUSE:SLE-15-SP7:Update, SUSE:SLE-15-SP6:Update"),
            "{err}"
        );
        assert!(select_codestreams(&available, &[]).is_err());
    }

    #[test]
    fn common_codestreams_are_those_of_every_package() {
        let a = vec![cs("A:Update"), cs("B:Update")];
        let b = vec![cs("B:Update"), cs("C:Update")];
        assert_eq!(
            common_codestreams(&[a.clone(), b]),
            vec!["B:Update".to_string()]
        );
        assert_eq!(
            common_codestreams(&[a]),
            vec!["A:Update".to_string(), "B:Update".to_string()]
        );
        assert!(common_codestreams(&[]).is_empty());
    }

    #[test]
    fn a_typed_pick_takes_numbers_names_or_all() {
        let p = vec!["A:Update".to_string(), "B:Update".to_string()];
        assert_eq!(pick_lines(&p), vec!["  1) A:Update", "  2) B:Update"]);
        assert_eq!(parse_pick("2", &p).unwrap(), vec!["B:Update"]);
        assert_eq!(parse_pick("1, B", &p).unwrap(), p);
        assert_eq!(parse_pick("ALL", &p).unwrap(), p);
        assert!(parse_pick("3", &p).is_err());
        assert!(parse_pick("  ", &p).is_err());
        assert!(parse_pick("C", &p).is_err());
    }

    #[test]
    fn the_newest_incident_copy_has_the_highest_number() {
        let listing = [
            "Botan",
            "Botan.19461",
            "Botan.2000",
            "Botan-doc.99999",
            "Botan.x1",
            "curl.5",
        ];
        assert_eq!(
            newest_incident_in(listing, "Botan").as_deref(),
            Some("Botan.19461")
        );
        assert_eq!(newest_incident_in(["Botan"], "Botan"), None);
        assert_eq!(without_incident_suffix("Botan.19461"), "Botan");
        assert_eq!(
            without_incident_suffix("python-ruamel.yaml"),
            "python-ruamel.yaml"
        );
    }

    #[test]
    fn a_package_without_an_update_is_browsed_in_the_linked_project() {
        let meta = r#"<project name="SUSE:SLE-15-SP7:Update" kind="maintenance_release">
  <link project="SUSE:SLE-15-SP7:GA"/>
  <repository name="standard">
    <path project="SUSE:SLE-15-SP7:GA" repository="standard"/>
  </repository>
</project>"#;
        assert_eq!(parse_project_links(meta), vec!["SUSE:SLE-15-SP7:GA"]);
        assert_eq!(
            planning_source("SUSE:SLE-15-SP7:Update", true, meta).as_deref(),
            Some("SUSE:SLE-15-SP7:Update")
        );
        assert_eq!(
            planning_source("SUSE:SLE-15-SP7:Update", false, meta).as_deref(),
            Some("SUSE:SLE-15-SP7:GA")
        );
        assert_eq!(
            planning_source("X:Update", false, "<project name=\"X:Update\"/>"),
            None
        );
        assert!(source_kind_allowed("maintenance_release"));
        assert!(!source_kind_allowed("maintenance_incident"));
    }

    #[test]
    fn branches_are_named_like_osc_branch_and_cleaned_up_only_in_home_branches() {
        assert_eq!(
            branch_project_for("u", "SUSE:SLE-15-SP7:Update"),
            "home:u:branches:SUSE:SLE-15-SP7:Update"
        );
        assert_eq!(
            sourceupdate_for("home:u:branches:SUSE:SLE-15-SP7:Update", Some("u")),
            Some("cleanup")
        );
        assert_eq!(sourceupdate_for("home:u:work", Some("u")), None);
        assert_eq!(sourceupdate_for("home:u:branches:X", None), None);
    }

    #[test]
    fn bug_references_are_checked_and_spelled_the_suse_way() {
        assert_eq!(parse_bug_ref("BSC#1234567").unwrap(), "bsc#1234567");
        assert_eq!(parse_bug_ref("boo#123456").unwrap(), "boo#123456");
        assert_eq!(parse_bug_ref("jsc#sle-12345").unwrap(), "jsc#SLE-12345");
        assert_eq!(parse_bug_ref("jsc#PED-1234").unwrap(), "jsc#PED-1234");
        assert_eq!(parse_bug_ref("cve-2025-12345").unwrap(), "CVE-2025-12345");
        for bad in [
            "bsc#12",
            "bsc1234567",
            "CVE-25-1234",
            "CVE-2025-12",
            "jsc#123",
            "gh#a/b#1",
            "",
        ] {
            assert!(parse_bug_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_maintenance_request_needs_a_reference_unless_told_otherwise() {
        let got = parse_bug_refs(
            &["bsc#1234567,CVE-2025-1234".into(), "BSC#1234567".into()],
            false,
        )
        .unwrap();
        assert_eq!(got, vec!["bsc#1234567", "CVE-2025-1234"]);
        assert!(
            parse_bug_refs(&[], false)
                .unwrap_err()
                .contains("--no-bug-ref")
        );
        assert!(parse_bug_refs(&[], true).unwrap().is_empty());
        assert!(parse_bug_refs(&["bsc#1".into()], true).is_err());
        assert_eq!(bug_ref_warnings(&["CVE-2025-1234".into()]).len(), 1);
        assert!(bug_ref_warnings(&["CVE-2025-1234".into(), "bsc#1234567".into()]).is_empty());
        assert_eq!(refs_suffix(&got), "(bsc#1234567, CVE-2025-1234)");
    }

    const CHANGES: &str = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 1.2.3:
  * fix a crash
  * new option

-------------------------------------------------------------------
Mon Sep  1 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 1.2.2 (bsc#1111111)
";

    #[test]
    fn references_go_to_the_end_of_the_first_bullet_before_its_colon() {
        let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
        let got = with_refs_in_newest_entry(CHANGES, &refs).unwrap();
        assert!(
            got.contains("- Update to 1.2.3 (bsc#1234567, CVE-2025-1234):\n  * fix a crash"),
            "{got}"
        );
        assert!(got.ends_with("- Update to 1.2.2 (bsc#1111111)\n"));
        // Idempotent: a second run adds nothing.
        assert_eq!(with_refs_in_newest_entry(&got, &refs), None);
        // Only the missing ones are added, and an older entry does not count.
        let got = with_refs_in_newest_entry(CHANGES, &["bsc#1111111".into()]).unwrap();
        assert!(got.contains("- Update to 1.2.3 (bsc#1111111):"), "{got}");
    }

    #[test]
    fn references_follow_a_wrapped_bullet_and_an_entry_without_one_gets_its_own() {
        let wrapped = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Fix the parser so that a long line
  no longer crashes it
- Second change
";
        let got = with_refs_in_newest_entry(wrapped, &["bsc#1234567".into()]).unwrap();
        assert!(
            got.contains("  no longer crashes it (bsc#1234567)\n- Second change"),
            "{got}"
        );
        let bare = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

Rebuilt.
";
        let got = with_refs_in_newest_entry(bare, &["bsc#1234567".into()]).unwrap();
        assert!(
            got.contains("<dev@suse.com>\n- References: bsc#1234567\n"),
            "{got}"
        );
        assert_eq!(
            with_refs_in_newest_entry("no entries", &["bsc#1234567".into()]),
            None
        );
    }

    #[test]
    fn a_reference_counts_only_as_a_whole() {
        assert!(mentions("fixed (bsc#123456)", "bsc#123456"));
        assert!(!mentions("fixed (bsc#1234567)", "bsc#123456"));
        assert!(mentions("CVE-2025-1234, x", "cve-2025-1234"));
        assert_eq!(
            missing_refs("bsc#1234567", &["bsc#1234567".into(), "CVE-2025-1".into()]),
            vec!["CVE-2025-1"]
        );
    }

    fn request() -> MaintenanceRequest {
        MaintenanceRequest {
            target_project: "SUSE:Maintenance".into(),
            actions: vec![
                IncidentAction {
                    source_project: "home:u:branches:SUSE:SLE-15-SP7:Update".into(),
                    source_package: "curl".into(),
                    release_project: "SUSE:SLE-15-SP7:Update".into(),
                },
                IncidentAction {
                    source_project: "home:u:branches:SUSE:SLE-15-SP6:Update".into(),
                    source_package: "curl".into(),
                    release_project: "SUSE:SLE-15-SP6:Update".into(),
                },
            ],
            sourceupdate: Some("cleanup".into()),
            description: "- Update to 8.1 (bsc#1234567) & more <x>".into(),
        }
    }

    #[test]
    fn the_request_has_one_incident_action_per_code_stream() {
        let x = request_xml(&request());
        assert_eq!(
            x.matches("<action type=\"maintenance_incident\">").count(),
            2
        );
        assert!(x.contains(
            "    <source project=\"home:u:branches:SUSE:SLE-15-SP7:Update\" package=\"curl\"/>\n    \
             <target project=\"SUSE:Maintenance\" releaseproject=\"SUSE:SLE-15-SP7:Update\"/>\n    \
             <options>\n      <sourceupdate>cleanup</sourceupdate>\n    </options>\n"
        ), "{x}");
        assert!(x.contains(
            "<description>- Update to 8.1 (bsc#1234567) &amp; more &lt;x&gt;</description>"
        ));
        let mut kept = request();
        kept.sourceupdate = None;
        assert!(!request_xml(&kept).contains("<options>"));
        assert_eq!(CREATE_REQUEST_PATH, "/request?cmd=create&addrevision=1");
    }

    #[test]
    fn open_incidents_are_found_per_source_and_release_project() {
        assert_eq!(
            open_incidents_path("home:u:branches:X:Update", "curl"),
            "/request?view=collection&roles=source&types=maintenance_incident&states=new,review,accepted&project=home%3Au%3Abranches%3AX%3AUpdate&package=curl"
        );
        let xml = r#"<collection matches="2"><request id="1382124" creator="u">
  <action type="maintenance_incident">
    <source project="home:u:branches:X:Update" package="curl" rev="c9"/>
    <target project="openSUSE:Maintenance" releaseproject="X:Update"/>
  </action>
  <state name="new" who="bot" when="2026-10-02T14:10:21"/>
</request>
<request id="7">
  <action type="submit">
    <source project="home:u:branches:X:Update" package="curl"/>
    <target project="X" package="curl"/>
  </action>
  <state name="review"/>
</request></collection>"#;
        let open = parse_open_incidents(xml);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "1382124");
        assert_eq!(open[0].state, "new");
        assert_eq!(open[0].release_project.as_deref(), Some("X:Update"));
        assert_eq!(open[0].rev.as_deref(), Some("c9"));
        assert!(
            find_open_incident(&open, "home:u:branches:X:Update", "curl", "X:Update", &[])
                .is_some()
        );
        assert!(
            find_open_incident(&open, "home:u:branches:X:Update", "curl", "Y:Update", &[])
                .is_none()
        );
        assert!(
            find_open_incident(&open, "home:u:branches:X:Update", "wget", "X:Update", &[])
                .is_none()
        );
    }

    #[test]
    fn the_description_is_the_changelog_entry_with_its_references() {
        let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
        let entry = "- Update to 1.2.3 (bsc#1234567, CVE-2025-1234):\n  * fix".to_string();
        let same = [
            ("A".to_string(), Some(entry.clone())),
            ("B".to_string(), Some(entry.clone())),
        ];
        let d = request_description("curl", "", &same, &refs);
        assert!(d.starts_with(&entry), "{d}");
        assert!(!d.contains("References:"), "{d}");
        let differ = [
            ("A".to_string(), Some(entry.clone())),
            ("B".to_string(), Some("- Other".into())),
        ];
        let d = request_description("curl", "", &differ, &refs);
        assert!(
            d.starts_with("A:\n- Update to 1.2.3") && d.contains("\n\nB:\n- Other"),
            "{d}"
        );
        let d = request_description("curl", "", &[("A".into(), None)], &refs);
        assert!(
            d.starts_with("Maintenance update of curl.\n\nReferences: bsc#1234567, CVE-2025-1234"),
            "{d}"
        );
    }

    #[test]
    fn the_plan_names_every_code_stream_the_references_and_what_is_left_out() {
        let lines = plan_lines(
            &request(),
            &["bsc#1234567".into()],
            "",
            &[],
            &[(
                "SUSE:SLE-12-SP5:Update".into(),
                "request 9 is already open".into(),
            )],
        );
        assert_eq!(
            lines[0],
            "maintenance request to SUSE:Maintenance (2 code streams):"
        );
        assert!(lines.contains(&"  home:u:branches:SUSE:SLE-15-SP7:Update/curl -> release in SUSE:SLE-15-SP7:Update".to_string()));
        assert!(lines.contains(
            &"  SUSE:SLE-12-SP5:Update: left out, request 9 is already open".to_string()
        ));
        assert!(lines.contains(&"  bug references: bsc#1234567".to_string()));
        assert!(lines.contains(&"  on accept: the branch package is removed".to_string()));
        assert!(lines.iter().any(|l| l.starts_with("    | - Update to 8.1")));
        let none = plan_lines(&request(), &[], "", &[], &[]);
        assert!(none.contains(&"  bug references: none (--no-bug-ref)".to_string()));
    }

    #[test]
    fn the_mode_defaults_to_backport_and_lint_takes_none() {
        assert_eq!(Mode::parse(None, false).unwrap(), Mode::Backport);
        assert_eq!(Mode::parse(Some("UPDATE"), false).unwrap(), Mode::Update);
        assert_eq!(
            Mode::parse(Some("backport"), false).unwrap(),
            Mode::Backport
        );
        assert_eq!(Mode::parse(None, true).unwrap(), Mode::Lint);
        assert!(Mode::parse(Some("update"), true).is_err());
        assert!(
            Mode::parse(Some("bump"), false)
                .unwrap_err()
                .contains("bump")
        );
    }

    #[test]
    fn backport_flags_must_fit_the_mode() {
        let one = vec!["curl".to_string()];
        let fix = vec!["fix.patch".to_string()];
        assert!(validate_mode(Mode::Backport, &fix, None, false, &one).is_ok());
        assert!(
            validate_mode(Mode::Backport, &[], None, false, &one)
                .unwrap_err()
                .contains("--backport")
        );
        assert!(
            validate_mode(Mode::Backport, &fix, None, true, &one)
                .unwrap_err()
                .contains("--no-bug-ref")
        );
        let two = vec!["curl".to_string(), "wget".to_string()];
        assert!(
            validate_mode(Mode::Backport, &fix, None, false, &two)
                .unwrap_err()
                .contains("one fix of one package")
        );
        assert!(validate_mode(Mode::Update, &fix, None, false, &two).is_err());
        assert!(
            validate_mode(Mode::Lint, &[], Some("https://github.com/o/r"), false, &one).is_err()
        );
        assert!(validate_mode(Mode::Update, &[], None, true, &two).is_ok());
        assert!(
            validate_mode(
                Mode::Backport,
                &fix,
                Some("https://github.com/o/r"),
                false,
                &one
            )
            .is_ok()
        );
    }

    fn job() -> backport::Job {
        backport::Job {
            patches: vec![backport::PreparedPatch {
                file_name: "CVE-2025-1234.patch".into(),
                bytes: b"--- a\n+++ b\n@@ -1 +1 @@\n".to_vec(),
                origin: "https://github.com/o/r/commit/abc1234.patch".into(),
                commit: Some("abc1234".into()),
                reason: "reject overlong header lines".into(),
            }],
            refs: vec!["bsc#1234567".into(), "CVE-2025-1234".into()],
        }
    }

    #[test]
    fn the_plan_and_the_description_say_which_mode() {
        let j = job();
        let backport = mode_line(Mode::Backport, Some(&j));
        assert_eq!(
            backport,
            "Backport of CVE-2025-1234.patch (https://github.com/o/r/commit/abc1234.patch) to the \
             version each code stream has (no version update)."
        );
        let update = mode_line(Mode::Update, None);
        assert!(update.starts_with("Version update"), "{update}");
        assert!(mode_line(Mode::Lint, None).contains("keeps its version"));
        let d = request_description("curl", &backport, &[("A".into(), Some("- x".into()))], &[]);
        assert!(d.starts_with("Backport of CVE-2025-1234.patch"), "{d}");
        assert!(d.contains("\n\n- x\n\n"), "{d}");
        let lines = plan_lines(
            &request(),
            &j.refs,
            &backport,
            &[(
                "SUSE:SLE-15-SP7:Update".into(),
                "8.6.0: CVE-2025-1234.patch applied after a refresh for this code stream".into(),
            )],
            &[],
        );
        assert_eq!(lines[1], format!("  mode: {backport}"));
        assert!(lines.contains(&"  home:u:branches:SUSE:SLE-15-SP7:Update/curl -> release in SUSE:SLE-15-SP7:Update: 8.6.0: CVE-2025-1234.patch applied after a refresh for this code stream".to_string()), "{lines:?}");
        assert!(lines.contains(&"  home:u:branches:SUSE:SLE-15-SP6:Update/curl -> release in SUSE:SLE-15-SP6:Update".to_string()), "{lines:?}");
    }

    #[test]
    fn a_backport_keeps_the_version_of_the_code_stream() {
        assert_eq!(version_kept(Some("8.6.0"), Some("8.6.0")).unwrap(), "8.6.0");
        assert!(
            version_kept(Some("8.6.0"), Some("8.11.0"))
                .unwrap_err()
                .contains("from 8.6.0 to 8.11.0")
        );
        assert!(version_kept(None, Some("1")).is_err());
        assert!(version_kept(Some("1"), None).is_err());
    }

    #[test]
    fn the_changelog_gets_the_backport_bullet_and_the_missing_references() {
        let j = job();
        let bullets = backport_bullets(Some(&j), &j.refs);
        assert_eq!(
            bullets,
            vec![(
                "CVE-2025-1234.patch".to_string(),
                "- Add CVE-2025-1234.patch: reject overlong header lines (bsc#1234567, CVE-2025-1234)"
                    .to_string()
            )]
        );
        let changes = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Add CVE-2025-1234.patch to fix the parser

-------------------------------------------------------------------
Mon Sep  1 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 8.6.0
";
        let got = amend_changes(changes, &j.refs, &bullets).unwrap();
        assert!(
            got.contains(&format!("<dev@suse.com>\n\n{}\n", bullets[0].1)),
            "{got}"
        );
        assert_eq!(got.matches("bsc#1234567").count(), 1, "{got}");
        assert_eq!(amend_changes(&got, &j.refs, &bullets), None);
        // An update run only adds the references.
        let got = amend_changes(changes, &["bsc#1234567".to_string()], &[]).unwrap();
        assert!(got.contains("fix the parser (bsc#1234567)"), "{got}");
        assert!(backport_bullets(None, &j.refs).is_empty());
    }

    struct Fixed(Vec<Affected>);
    impl StreamCheck for Fixed {
        fn check(&self, _: &str, _: &[String], _: &FixQuery<'_>) -> Vec<Affected> {
            self.0.clone()
        }
    }

    #[test]
    fn the_stream_check_seam_filters_only_what_it_rules_out() {
        let streams = vec![
            "A:Update".to_string(),
            "B:Update".to_string(),
            "C:Update".to_string(),
        ];
        let refs = vec!["CVE-2025-1234".to_string()];
        let fix = FixQuery {
            refs: &refs,
            sources: &[],
        };
        let none = verdicts(&NoStreamCheck, "curl", &streams, &fix);
        assert!(none.iter().all(|(_, a)| *a == Affected::Unknown));
        assert_eq!(streams_to_build(&streams, &[none]), streams);
        // A check that answers for fewer streams leaves the rest unknown.
        let some = verdicts(
            &Fixed(vec![
                Affected::Affected,
                Affected::NotAffected("the code is not in 1.0".into()),
            ]),
            "curl",
            &streams,
            &fix,
        );
        assert_eq!(some[2].1, Affected::Unknown);
        assert_eq!(
            streams_to_build(&streams, std::slice::from_ref(&some)),
            vec!["A:Update".to_string(), "C:Update".to_string()]
        );
        // With several packages a stream is left out only when none needs it.
        let all = vec![(streams[1].clone(), Affected::Unknown)];
        assert_eq!(streams_to_build(&streams, &[some, all]), streams);
    }

    #[test]
    fn the_affected_by_command_gets_package_stream_and_fix_and_its_answer_is_parsed() {
        let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
        let sources = vec!["https://x/y.patch".to_string()];
        let fix = FixQuery {
            refs: &refs,
            sources: &sources,
        };
        assert_eq!(
            affected_by_args("curl", "SUSE:SLE-15-SP7:Update", &fix),
            vec![
                "curl",
                "SUSE:SLE-15-SP7:Update",
                "bsc#1234567,CVE-2025-1234",
                "https://x/y.patch"
            ]
        );
        assert_eq!(parse_affected("affected\n", true), Affected::Affected);
        assert_eq!(
            parse_affected("\nnot-affected  the code came in 8.10\n", true),
            Affected::NotAffected("the code came in 8.10".into())
        );
        assert!(
            matches!(parse_affected("not-affected", true), Affected::NotAffected(w) if !w.is_empty())
        );
        assert_eq!(parse_affected("unknown", true), Affected::Unknown);
        assert_eq!(parse_affected("affected", false), Affected::Unknown);
        assert_eq!(parse_affected("yes", true), Affected::Unknown);
        assert_eq!(parse_affected("affected maybe", true), Affected::Unknown);
        assert_eq!(parse_affected("", true), Affected::Unknown);
        assert_eq!(
            parse_affected("not-affected a\u{1b}[31mb", true),
            Affected::NotAffected("a[31mb".into())
        );
    }

    #[test]
    fn an_affected_by_command_that_fails_or_cannot_start_is_unknown() {
        let refs = vec!["CVE-2025-1234".to_string()];
        let fix = FixQuery {
            refs: &refs,
            sources: &[],
        };
        let missing = CommandCheck {
            program: "/nonexistent/affected-by-check".into(),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(
            missing.check("curl", &["A:Update".to_string()], &fix),
            vec![Affected::Unknown]
        );
    }

    #[test]
    fn every_code_stream_ends_in_the_summary_and_the_plan() {
        let reports = vec![
            StreamReport {
                stream: "A:Update".into(),
                outcome: StreamOutcome::Ready("8.6.0: CVE-2025-1234.patch applied cleanly".into()),
            },
            StreamReport {
                stream: "B:Update".into(),
                outcome: StreamOutcome::Failed(
                    "the backport did not go in: the branch has no CVE-2025-1234.patch".into(),
                ),
            },
            StreamReport {
                stream: "C:Update".into(),
                outcome: StreamOutcome::NotAffected("the code came in 8.10".into()),
            },
            StreamReport {
                stream: "D:Update".into(),
                outcome: StreamOutcome::LeftOut(
                    "request 9 from home:u:branches:D:Update is already open".into(),
                ),
            },
            StreamReport {
                stream: "[git] pool/curl:leap-16.0".into(),
                outcome: StreamOutcome::GitMaintained,
            },
        ];
        let s = summary_lines("curl", &reports);
        assert_eq!(
            s[0],
            "curl: 1 of 5 code streams ready for the maintenance request:"
        );
        assert_eq!(
            s[1],
            "  A:Update: ready, 8.6.0: CVE-2025-1234.patch applied cleanly"
        );
        assert!(
            s[2].starts_with("  B:Update: FAILED, the backport did not go in"),
            "{s:?}"
        );
        assert_eq!(s[3], "  C:Update: not affected, the code came in 8.10");
        assert!(s[4].starts_with("  D:Update: left out, request 9"), "{s:?}");
        assert_eq!(
            s[5],
            "  [git] pool/curl:leap-16.0: skipped: maintained in git (SLFO), which is not supported yet"
        );
        let out = left_out(&reports);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|(s, _)| s != "A:Update"));
        assert_eq!(
            out[0].1,
            "failed: the backport did not go in: the branch has no CVE-2025-1234.patch"
        );
        assert_eq!(out[3].1, "maintained in git (SLFO), not supported yet");
    }

    #[test]
    fn git_maintained_streams_are_read_from_osc_maintained() {
        let text = "openSUSE:Backports:SLE-15-SP7:Update/curl\n\
                    [git] pool/curl:leap-16.0\n\
                    [git] products/curl:slfo-main\n\
                    [git] broken\n";
        let got = parse_git_maintained(text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].to_string(), "[git] pool/curl:leap-16.0");
        assert_eq!(got[1].branch, "slfo-main");
        assert!(parse_git_maintained("A:Update/curl\n").is_empty());
    }

    #[test]
    fn an_accepted_request_counts_only_at_the_current_source_revision() {
        // The shape OBS returns: the accepted request's target is the incident
        // and `rev` is the branch link's expanded srcmd5 (request 1249558).
        let xml = r#"<collection matches="2">
<request id="11" creator="u">
  <action type="maintenance_incident">
    <source project="home:u:branches:X:Update" package="curl" rev="30e3f10b756fafa3289949583302ec27"/>
    <target project="openSUSE:Maintenance:19468" releaseproject="X:Update"/>
  </action>
  <state name="accepted" who="m" when="2026-10-02T14:10:21"/>
</request>
<request id="12" creator="u">
  <action type="maintenance_incident">
    <source project="home:u:branches:X:Update" package="curl" rev="aaaa"/>
    <target project="openSUSE:Maintenance:19000" releaseproject="X:Update"/>
  </action>
  <state name="accepted" who="m"/>
</request></collection>"#;
        let found = parse_open_incidents(xml);
        assert_eq!(found.len(), 2);
        let dir = r#"<directory name="curl" rev="3" vrev="3" srcmd5="406bf488d1c3eb9bf362555e09055d5b">
  <linkinfo project="X:Update" package="curl" srcmd5="52c1" baserev="52c1" xsrcmd5="30e3f10b756fafa3289949583302ec27" lsrcmd5="406b"/>
  <entry name="_link" md5="8b" size="148" mtime="1"/>
</directory>"#;
        let current = parse_source_revisions(dir);
        assert_eq!(
            current,
            vec![
                "3".to_string(),
                "406bf488d1c3eb9bf362555e09055d5b".to_string(),
                "30e3f10b756fafa3289949583302ec27".to_string()
            ]
        );
        let hit = find_open_incident(
            &found,
            "home:u:branches:X:Update",
            "curl",
            "X:Update",
            &current,
        )
        .unwrap();
        assert_eq!(hit.id, "11");
        assert_eq!(
            submitted_reason(hit, "home:u:branches:X:Update"),
            "request 11 already submitted this revision of home:u:branches:X:Update (accepted)"
        );
        // A newer branch revision is a new change: the accepted ones do not count.
        assert!(
            find_open_incident(
                &found,
                "home:u:branches:X:Update",
                "curl",
                "X:Update",
                &["bbbb".into()]
            )
            .is_none()
        );
        // An accepted request without a pinned revision proves nothing.
        let mut unpinned = found.clone();
        unpinned.iter_mut().for_each(|o| o.rev = None);
        assert!(
            find_open_incident(
                &unpinned,
                "home:u:branches:X:Update",
                "curl",
                "X:Update",
                &current
            )
            .is_none()
        );
        let open = OpenIncident {
            state: "review".into(),
            ..found[1].clone()
        };
        assert_eq!(
            submitted_reason(&open, "B"),
            "request 12 from B is already open"
        );
        assert!(parse_source_revisions("<nope/>").is_empty());
    }

    #[test]
    fn a_maintenance_run_refuses_build_paths_with_a_clear_reason() {
        assert_eq!(build_path_refusal("curl", &[]), None);
        let why = build_path_refusal("curl", &["devel:libraries:c_c++/SLE_15_SP7".into()]).unwrap();
        assert!(
            why.starts_with("curl needs the build path devel:libraries:c_c++/SLE_15_SP7"),
            "{why}"
        );
        assert!(why.contains("not added"), "{why}");
    }

    #[test]
    fn a_running_incident_is_named_as_one() {
        let w = devel_warning(
            "openSUSE:Backports:SLE-15-SP7:Update",
            "tor",
            &(
                "openSUSE:Maintenance:19709".into(),
                "tor.openSUSE_Backports_SLE-15-SP7_Update".into(),
            ),
        );
        assert!(w.contains("an incident"), "{w}");
        let w = devel_warning("L:Update", "curl", &("S:Update".into(), "curl".into()));
        assert!(w.contains("a fix normally goes to that code stream"), "{w}");
    }
}
