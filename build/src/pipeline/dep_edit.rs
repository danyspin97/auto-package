//! Editing the dependencies of a spec: the dependency keys and workspaces, the
//! newest releases within a bound, and the dependency entries it adds or drops.

use super::*;

/// The key a dependency's workspace is found by: its name with the Python
/// prefix stripped once, compared without case or separators. `python-APScheduler`,
/// `APScheduler` and `python-apscheduler` share one; `ffmpeg-8` keeps its suffix
/// and matches `python-ffmpeg-8`.
pub(super) fn dependency_key(name: &str) -> String {
    osc::fuzzy_key(osc::strip_rpm_prefix(name))
}

/// The workspace of dependency `name` under whatever spelling its request used.
/// An exact workspace name (the RPM guess, then the name itself) wins; otherwise
/// the first workspace directory, in sorted order, with the same [`dependency_key`].
pub(super) fn dependency_workspace(state_root: &Path, name: &str) -> Option<Workspace> {
    if let Some(ws) = Workspace::find_existing(state_root, &[&osc::rpm_name(name), name]) {
        return Some(ws);
    }
    let key = dependency_key(name);
    let mut dirs: Vec<String> = std::fs::read_dir(state_root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .find(|d| dependency_key(d) == key)
        .and_then(|d| Workspace::find_existing(state_root, &[&d]))
}

/// The newest of `versions` that `specifier` (`<=0.1.11,>=0.1.10`) allows,
/// leaving out pre-releases. `None` when the bound cannot be read or nothing
/// meets it.
pub(super) fn newest_within<'a>(
    versions: impl Iterator<Item = &'a str>,
    specifier: &str,
) -> Option<String> {
    let bound = crate::distro_bounds::parse_specifier(specifier)?;
    versions
        .filter_map(|v| {
            let parsed = <pep440_rs::Version as std::str::FromStr>::from_str(v).ok()?;
            (!parsed.any_prerelease() && bound.contains(&parsed)).then_some((parsed, v))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, v)| v.to_string())
}

/// The newest PyPI release of `name` that `specifier` allows.
pub(super) fn newest_pypi_release_within(name: &str, specifier: &str) -> Option<String> {
    let url = format!("https://pypi.org/pypi/{}/json", urlencoding::encode(name));
    let body = crate::http::read_text(crate::http::get_with_retries(&url).ok()?).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let releases = json["releases"].as_object()?;
    // A release with every file yanked cannot be installed.
    let live = releases.iter().filter(|(_, files)| {
        files
            .as_array()
            .is_some_and(|f| f.iter().any(|x| x["yanked"] != true))
    });
    newest_within(live.map(|(v, _)| v.as_str()), specifier)
}

/// Say, one warning per name, why `skipped` names are not queued.
pub(super) fn warn_skipped(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    skipped: &[(String, crate::depclass::Skip)],
) {
    for (name, why) in skipped {
        ctx.reporter.report(&Event::Warning {
            step: Some(step),
            message: format!("not queuing {name} as missing: {}", why.reason()),
        });
    }
}

/// What the own project and the projects it builds against ship, as the
/// availability check reads it.
pub(super) struct ProjectView {
    pub(super) listing: std::sync::Arc<std::collections::HashSet<String>>,
    pub(super) statuses: std::collections::HashMap<String, osc::BuildStatus>,
    pub(super) linked: Vec<(String, std::sync::Arc<std::collections::HashSet<String>>)>,
}

/// The listings and build statuses of the own project and every linked one, or
/// `None` when any of them cannot be read: the callers decide what not knowing
/// means for them.
fn project_view(ctx: &RunCtx, step: ap_core::event::StepId) -> Option<ProjectView> {
    let config = ctx.config;
    let emit = Emit::new(ctx.reporter, step);
    let listing = osc::try_list_project_packages(&config.project, emit).ok()?;
    let statuses =
        osc::get_project_build_statuses(&config.project, &config.distro, &config.arch).ok()?;
    let mut linked = Vec::new();
    for (path_project, _repo) in osc::get_project_repo_paths(&config.project, emit).ok()? {
        let l = osc::try_list_project_packages(&path_project, emit).ok()?;
        linked.push((path_project, l));
    }
    Some(ProjectView {
        listing,
        statuses,
        linked,
    })
}

/// A spec's `BuildRequires:` and `Requires:` entries: a `Requires:` line is
/// read the way a `BuildRequires:` one is, so the splitting (bounds, `%{...}`,
/// commas) is the one `spec::build_requires` already does.
fn spec_dependency_entries(spec: &str) -> Vec<crate::spec::BuildRequire> {
    let both: String = spec
        .lines()
        .map(|l| match l.trim_start() {
            t if t.starts_with("Requires") => format!("Build{t}\n"),
            _ => format!("{l}\n"),
        })
        .collect();
    crate::spec::build_requires(&both)
}

/// `python-<x>` for a dependency entry that is clearly a Python package: a
/// `python-`/`python3-`/`python313-` name or `%{python_module x}`. Everything
/// else (a capability, a `-devel` package, a name with a macro in it) is `None`.
fn python_dependency(entry: &crate::spec::BuildRequire) -> Option<String> {
    use crate::spec::BuildRequire;
    let name = match entry {
        BuildRequire::Package(n) => {
            let bare = osc::strip_rpm_prefix(n);
            if bare == n || bare.ends_with("-devel") {
                return None;
            }
            bare
        }
        BuildRequire::Capability(token) => token
            .strip_prefix("%{python_module")
            .and_then(|r| r.strip_suffix('}'))
            .filter(|r| r.starts_with(char::is_whitespace))
            .and_then(|r| r.split_whitespace().next())?,
    };
    let plain = name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    plain.then(|| format!("python-{name}"))
}

/// The Python packages `after` depends on and `before` did not, as
/// `python-<x>`, compared by [`osc::fuzzy_key`] so a respelling is not an
/// addition.
pub(super) fn added_python_dependencies(before: &str, after: &str) -> Vec<String> {
    let known: std::collections::HashSet<String> = spec_dependency_entries(before)
        .iter()
        .filter_map(python_dependency)
        .map(|d| osc::fuzzy_key(&d))
        .collect();
    let mut seen = known;
    spec_dependency_entries(after)
        .iter()
        .filter_map(python_dependency)
        .filter(|d| seen.insert(osc::fuzzy_key(d)))
        .collect()
}

/// `names` minus the ones the own project (except a failed build) or a linked
/// project ships.
pub(super) fn not_shipped(names: Vec<String>, view: &ProjectView) -> Vec<String> {
    use crate::depclass::{Availability, availability};
    names
        .into_iter()
        .filter(|n| {
            matches!(
                availability(n, &view.listing, &view.statuses, &view.linked),
                Availability::Absent
                    | Availability::InProject {
                        status: Some(osc::BuildStatus::Failed),
                        ..
                    }
            )
        })
        .collect()
}

/// The `MissingDeps` result for a Python package a turn's edit added to the spec
/// that no project this one builds against ships, or `None` when there is none
/// (or the listings cannot be read: not knowing does not block the build).
///
/// `before` is the spec the turn started from; the edited one is on disk and
/// stays there, so it is applied when the dependency is built. Without this the
/// package is committed and built, OBS reports the new dependency unresolvable
/// after a full round (python-cuda-core added `cuda-bindings`), and only then
/// is it queued.
pub(super) fn missing_deps_added_by_edit(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    before: &str,
) -> Option<PipelineResult> {
    let after = std::fs::read_to_string(&ctx.spec_path).ok()?;
    let own = |d: &str| {
        let key = osc::fuzzy_key(osc::strip_rpm_prefix(d));
        [ctx.config.pkg_name.as_str(), ctx.rpm_name.as_str()]
            .iter()
            .any(|n| osc::fuzzy_key(osc::strip_rpm_prefix(n)) == key)
    };
    let added: Vec<String> = added_python_dependencies(before, &after)
        .into_iter()
        .filter(|d| !own(d))
        .collect();
    if added.is_empty() {
        return None;
    }
    // Capabilities, interpreter packages and system tools are not packages to
    // build; a PyPI name is never a bare capability of the distro, so nothing
    // here asks it.
    let (named, _skipped) = crate::depclass::triage_all(&added, &|_| false);
    if named.is_empty() {
        return None;
    }
    let to_build = not_shipped(named, &project_view(ctx, step)?);
    if to_build.is_empty() {
        return None;
    }
    ctx.reporter.report(&Event::MissingPackages {
        step,
        names: to_build.clone(),
        source: ap_core::event::MissingSource::Agent,
    });
    ctx.reporter.report(&Event::Warning {
        step: Some(step),
        message: format!(
            "the edit added {} to the spec and neither {} nor a project it builds against \
             ships it: queuing it before the build instead of waiting for OBS to report it \
             unresolvable",
            to_build.join(", "),
            ctx.config.project
        ),
    });
    Some(PipelineResult::MissingDeps {
        rpm_name: ctx.rpm_name.clone(),
        deps: to_build,
        patches: Vec::new(),
        native: Vec::new(),
        outdated: Vec::new(),
    })
}

/// `deps`, minus the ones the project already has built for this repository
/// and architecture and the ones a linked project ships.
///
/// An agent that is told a build failed decides what is missing from the log
/// and from what it can see on the host, and the host's repositories are not
/// the project's: it called `fastapi-cloud-cli` missing while OBS had it
/// built, and the package was then abandoned for a dependency that was never
/// missing (its edit, which named that dependency in BuildRequires, thrown
/// away unbuilt). A package OBS reports `succeeded` here is not missing; say
/// so, and when what it published is unusable (an unfinished skeleton spec)
/// say that instead, because that is the real problem. A listing that cannot
/// be read keeps every name: not knowing is not a refutation.
///
/// A linked project's package is only a stand-in for "built": it may be too
/// old for a bound, which is what `patch_dependencies` and a build of the
/// newer release are for, so the warning says what to do if it is not enough.
pub(super) fn without_available_deps(
    ctx: &RunCtx,
    step: ap_core::event::StepId,
    deps: Vec<String>,
) -> Vec<String> {
    if deps.is_empty() {
        return deps;
    }
    let config = ctx.config;
    let Some(ProjectView {
        listing,
        statuses,
        linked,
    }) = project_view(ctx, step)
    else {
        return deps;
    };
    deps.into_iter()
        .filter(|dep| {
            use crate::depclass::{Availability, availability};
            let message = match availability(dep, &listing, &statuses, &linked) {
                Availability::InProject {
                    pkg,
                    status: Some(osc::BuildStatus::Succeeded),
                } => {
                    match osc::empty_package_warning(
                        &config.project,
                        &pkg,
                        &config.distro,
                        &config.arch,
                    ) {
                        Some(why) => format!("not queuing {dep} as missing: {why}"),
                        None => format!(
                            "not queuing {dep} as missing: {pkg} is already built in {} for \
                             {}/{}, so the failure is not caused by it being absent",
                            config.project, config.distro, config.arch
                        ),
                    }
                }
                Availability::InLinked { project, pkg } => format!(
                    "not queuing {dep} as missing: {project} ships it as {pkg}, so a \
                     BuildRequires on it resolves (if its version is too old for a bound, \
                     report it in patch_dependencies or ask for the newer release instead)"
                ),
                _ => return true,
            };
            ctx.reporter.report(&Event::Warning {
                step: Some(step),
                message,
            });
            false
        })
        .collect()
}
