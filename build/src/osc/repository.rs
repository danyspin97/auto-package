//! Choosing the repository a package builds in.

use super::*;

/// The name `dep` has in `listing` when the project has it built
/// (`succeeded`) in the repository/architecture `statuses` was read for, else
/// `None`. Names are matched by [`project_key`], with or without the `python-`
/// prefix, and the listing's own spelling is returned.
pub fn built_in_project(
    listing: &std::collections::HashSet<String>,
    statuses: &HashMap<String, BuildStatus>,
    dep: &str,
) -> Option<String> {
    let bare = project_key(dep);
    let rpm = project_key(&rpm_name(dep));
    listing
        .iter()
        .filter(|p| {
            let k = project_key(p);
            k == bare || k == rpm
        })
        .find(|p| statuses.get(*p).is_some_and(BuildStatus::is_success))
        .cloned()
}

/// Whether `info` (a project meta) builds for `repo` on `arch`: the `Err`
/// says what the project does offer. A meta with no repository at all is
/// left to the "no build repositories" handling elsewhere.
pub(super) fn check_repo_arch(
    info: &ProjectInfo,
    project: &str,
    repo: &str,
    arch: &str,
) -> Result<(), String> {
    if info.repositories.is_empty() {
        return Ok(());
    }
    let Some(def) = info.repositories.iter().find(|r| r.name == repo) else {
        let names: Vec<&str> = info.repositories.iter().map(|r| r.name.as_str()).collect();
        return Err(format!(
            "{project} has no repository {repo} (it has: {})",
            names.join(", ")
        ));
    };
    if !def.arches.iter().any(|a| a == arch) {
        return Err(format!(
            "{project} does not build {repo} for {arch} (it builds: {})",
            def.arches.join(", ")
        ));
    }
    Ok(())
}

/// The repository of a project that stands for `wanted` on `arch`: `wanted`
/// itself when the project has it, else the one the distribution is also known
/// by (`openSUSE_Tumbleweed` is built as `openSUSE_Factory` in Factory's devel
/// projects, and a branch takes its repositories from the project it was
/// branched from), else the only repository that builds `arch`.
pub(super) fn pick_repository(
    info: &ProjectInfo,
    wanted: &str,
    arch: &str,
) -> Result<String, String> {
    let builds = |name: &str| {
        info.repositories
            .iter()
            .find(|r| r.name == name)
            .is_some_and(|r| r.arches.iter().any(|a| a == arch))
    };
    if builds(wanted) {
        return Ok(wanted.to_string());
    }
    let aliases: &[&str] = match wanted {
        "openSUSE_Tumbleweed" => &["openSUSE_Factory"],
        "openSUSE_Factory" => &["openSUSE_Tumbleweed"],
        _ => &[],
    };
    if let Some(alias) = aliases.iter().find(|a| builds(a)) {
        return Ok((*alias).to_string());
    }
    let mut serving = info
        .repositories
        .iter()
        .filter(|r| r.arches.iter().any(|a| a == arch));
    match (serving.next(), serving.next()) {
        (Some(only), None) => Ok(only.name.clone()),
        _ => Err(format!(
            "no repository for {wanted} on {arch} (it has: {})",
            info.repositories
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The repository to build for when none was asked for: Tumbleweed, else
/// Factory, else the one repository that builds `arch`, else the first by name.
/// The others that build `arch` come second, so the caller can say what it
/// passed over.
pub(super) fn default_repository(info: &ProjectInfo, arch: &str) -> Option<(String, Vec<String>)> {
    let mut serving: Vec<&str> = info
        .repositories
        .iter()
        .filter(|r| r.arches.iter().any(|a| a == arch))
        .map(|r| r.name.as_str())
        .collect();
    serving.sort_unstable();
    let chosen = ["openSUSE_Tumbleweed", "openSUSE_Factory"]
        .into_iter()
        .find(|p| serving.contains(p))
        .or_else(|| serving.first().copied())?
        .to_string();
    let others = serving
        .into_iter()
        .filter(|n| *n != chosen)
        .map(str::to_string)
        .collect();
    Some((chosen, others))
}

/// The repository of `project` to build for on `arch`: the one asked for, as
/// [`resolve_repository`] reads it, or, when `requested` is `None`, the
/// project's own preferred one. The second part is the other repositories that
/// build `arch` when the choice was between several. A project that does not
/// exist yet, or whose meta cannot be read, gets `openSUSE_Tumbleweed`.
pub fn choose_repository(
    project: &str,
    requested: Option<&str>,
    arch: &str,
) -> (String, Vec<String>) {
    const FALLBACK: &str = "openSUSE_Tumbleweed";
    if let Some(wanted) = requested {
        return (resolve_repository(project, wanted, arch), Vec::new());
    }
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return (FALLBACK.to_string(), Vec::new());
    };
    default_repository(&parse_project_info(&meta), arch)
        .unwrap_or_else(|| (FALLBACK.to_string(), Vec::new()))
}

/// [`pick_repository`] for a project on OBS, remembered for the run. A project
/// whose meta cannot be read keeps `wanted`, so the build's own check reports
/// what is wrong.
pub fn resolve_repository(project: &str, wanted: &str, arch: &str) -> String {
    /// (project, wanted, arch) → the repository chosen.
    type Known = HashMap<(String, String, String), String>;
    static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
    let key = (project.to_string(), wanted.to_string(), arch.to_string());
    if let Some(hit) = KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&key))
    {
        return hit.clone();
    }
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return wanted.to_string();
    };
    let info = parse_project_info(&meta);
    if info.repositories.is_empty() {
        return wanted.to_string();
    }
    let chosen = pick_repository(&info, wanted, arch).unwrap_or_else(|_| wanted.to_string());
    KNOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key, chosen.clone());
    chosen
}
