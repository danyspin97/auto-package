use super::*;

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
    /// The work directory the code streams' updates build in: each branch
    /// project keeps its state under it (see [`branch_state_root`]), where a
    /// build recorded the risk flags `--yes` must not submit past.
    pub work_dir: PathBuf,
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
pub(super) fn choose(
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
pub(super) fn read_changes(project: &str, pkg: &str, emit: Emit<'_>) -> Option<(String, String)> {
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
pub(super) struct Ready {
    pub(super) action: IncidentAction,
    /// The `.changes` file name and its text with the bullets and references
    /// added, when they are missing from the branch.
    pub(super) amended: Option<(String, String)>,
    pub(super) entry: Option<String>,
    /// How its change went, for the plan and the summary.
    pub(super) note: String,
}

pub(super) fn spec_version(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    crate::update::spec_for_package(project, pkg, emit)
        .ok()
        .as_deref()
        .and_then(crate::spec::parse_header)
        .map(|h| h.version)
}
