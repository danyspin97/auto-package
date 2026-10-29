use super::*;

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
