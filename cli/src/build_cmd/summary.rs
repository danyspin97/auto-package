//! The run summary printed when a build or update ends.

use crate::*;

/// Lists what this run built, what is queued and what the project already has
/// built for this distro and arch, so the outcome is readable without
/// scrolling the log.
/// The sections of the last rpmlint run in a `rpmlint.log`: every section
/// carrying the `unix time` of the last header, so each flavor of a multibuild
/// package counts, not only the one written last.
pub(crate) fn latest_rpmlint_run(text: &str) -> String {
    const HEADER: &str = "=== rpmlint of";
    let stamp = |section: &str| -> Option<String> {
        let header = section.lines().next()?;
        let at = header.rfind("unix time ")? + "unix time ".len();
        Some(header[at..].trim_end_matches([')', ' ', '=']).to_string())
    };
    let mut sections: Vec<&str> = Vec::new();
    let mut start = None;
    for (at, _) in text.match_indices(HEADER) {
        if (at == 0 || text.as_bytes()[at - 1] == b'\n')
            && let Some(s) = start.replace(at)
        {
            sections.push(&text[s..at]);
        }
    }
    if let Some(s) = start {
        sections.push(&text[s..]);
    }
    let Some(last) = sections.last().and_then(|s| stamp(s)) else {
        return String::new();
    };
    sections
        .iter()
        .filter(|s| stamp(s).as_deref() == Some(last.as_str()))
        .copied()
        .collect()
}

/// The error lines of the latest rpmlint run (all its flavors) in `state/<pkg>/rpmlint.log`
/// of each built package, as `pkg: line`.
pub(crate) fn rpmlint_errors_of(state_root: &Path, built: &[&String]) -> Vec<String> {
    let mut out = Vec::new();
    for pkg in built {
        let Some(ws) = ap_build::workspace::Workspace::find_existing(state_root, &[pkg.as_str()])
        else {
            continue;
        };
        let text = std::fs::read_to_string(ws.dir().join("rpmlint.log")).unwrap_or_default();
        let last = latest_rpmlint_run(&text);
        out.extend(
            ap_build::osc::rpmlint_errors(&last)
                .into_iter()
                .map(|l| format!("{pkg}: {l}")),
        );
    }
    out
}

/// A package's dependencies as the summary lists them: one that failed this run
/// reads `failed X`, so a package waiting on it is not mistaken for one that
/// will get its turn.
pub(crate) fn waiting_list(deps: &[String], failed: &[&String]) -> String {
    let key = |n: &str| ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(n));
    deps.iter()
        .map(|d| {
            if failed.iter().any(|b| key(b) == key(d)) {
                format!("failed {d}")
            } else {
                d.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The packages whose latest outcome is a failure this run, a needs-maintainer
/// stop included: a package waiting on one reads `failed X`.
pub(crate) fn failed_packages(
    latest: &std::collections::BTreeMap<String, (String, PackageOutcome)>,
) -> Vec<&String> {
    latest
        .iter()
        .filter_map(|(pkg, (_, outcome))| match outcome {
            PackageOutcome::Failed { .. } | PackageOutcome::NeedsMaintainer { .. } => Some(pkg),
            _ => None,
        })
        .collect()
}

/// The packages the fix loop changed: built, given up on or stopped for a
/// maintainer, so their flags and risks are listed.
pub(crate) fn touched_packages(
    latest: &std::collections::BTreeMap<String, (String, PackageOutcome)>,
) -> Vec<&String> {
    latest
        .iter()
        .filter(|(_, (_, o))| {
            matches!(
                o,
                PackageOutcome::Built
                    | PackageOutcome::BuiltLocally
                    | PackageOutcome::AlreadyBuilt
                    | PackageOutcome::MissingDeps { .. }
                    | PackageOutcome::Failed { .. }
                    | PackageOutcome::NeedsMaintainer { .. }
            )
        })
        .map(|(pkg, _)| pkg)
        .collect()
}

/// The packages among `pkgs` whose recorded fix-loop flags are not empty, with
/// each flag's line, in the order given.
pub(crate) fn fix_flagged<'a>(
    state_root: &Path,
    pkgs: impl Iterator<Item = &'a String>,
) -> Vec<(String, Vec<String>)> {
    with_lines(pkgs, |pkg| {
        ap_build::fix_flags::of_package(state_root, pkg)
            .iter()
            .map(|f| f.line())
            .collect()
    })
}

/// The packages among `pkgs` that `lines_of` gives lines for, with those
/// lines, in the order given.
pub(crate) fn with_lines<'a>(
    pkgs: impl Iterator<Item = &'a String>,
    lines_of: impl Fn(&str) -> Vec<String>,
) -> Vec<(String, Vec<String>)> {
    pkgs.filter_map(|pkg| {
        let lines = lines_of(pkg);
        (!lines.is_empty()).then(|| (pkg.clone(), lines))
    })
    .collect()
}

pub(crate) fn print_run_summary(
    memory: &MemorySink,
    project: &str,
    distro: &str,
    arch: &str,
    state_root: &Path,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    // Latest outcome per package, so a package that later built is not also
    // reported as queued, and a package listed twice is listed once.
    let mut latest: std::collections::BTreeMap<String, (String, PackageOutcome)> =
        Default::default();
    for ev in memory.finished() {
        if let Event::PackageFinished {
            pkg,
            version,
            outcome,
        } = ev
        {
            latest.insert(pkg, (version, outcome));
        }
    }

    let built: Vec<&String> = latest
        .iter()
        .filter(|(_, (_, outcome))| {
            matches!(
                outcome,
                PackageOutcome::Built | PackageOutcome::BuiltLocally
            )
        })
        .map(|(pkg, _)| pkg)
        .collect();
    let _ = writeln!(out, "\nBuilt this run ({}):", built.len());
    for pkg in &built {
        // A local build is not on OBS: say so beside it, never as a plain build.
        let local = if matches!(latest[*pkg].1, PackageOutcome::BuiltLocally) {
            " (built locally, not on OBS)"
        } else {
            ""
        };
        let _ = writeln!(out, "  {pkg} {}{local}", latest[*pkg].0);
    }

    let mut queued: Vec<(&String, Vec<String>)> = latest
        .iter()
        .filter_map(|(pkg, (_, outcome))| match outcome {
            PackageOutcome::MissingDeps { deps } => {
                let mut deps = deps.clone();
                deps.sort();
                deps.dedup();
                Some((pkg, deps))
            }
            _ => None,
        })
        .collect();
    queued.retain(|(pkg, _)| !built.contains(pkg));
    let failed = failed_packages(&latest);
    let _ = writeln!(out, "\nQueued, waiting on dependencies ({}):", queued.len());
    for (pkg, deps) in queued {
        if deps.is_empty() {
            let _ = writeln!(out, "  {pkg}");
        } else {
            let _ = writeln!(out, "  {pkg} <- {}", waiting_list(&deps, &failed));
        }
    }

    // Stopped for a maintainer's decision (a refused signature): not a build
    // failure, and nothing was sent to OBS for it.
    let needs_maintainer: Vec<(&String, &String)> = latest
        .iter()
        .filter_map(|(pkg, (_, outcome))| match outcome {
            PackageOutcome::NeedsMaintainer { reason } => Some((pkg, reason)),
            _ => None,
        })
        .collect();
    if !needs_maintainer.is_empty() {
        let _ = writeln!(out, "\nNeeds a maintainer ({}):", needs_maintainer.len());
        for (pkg, reason) in needs_maintainer {
            let _ = writeln!(out, "  {pkg}: {reason}");
        }
    }

    // Excluded by OBS for the build's repository and arch: nothing was built
    // there, and that is not a failure.
    let skipped: Vec<(&String, &String)> = latest
        .iter()
        .filter_map(|(pkg, (_, outcome))| match outcome {
            PackageOutcome::Skipped { reason } => Some((pkg, reason)),
            _ => None,
        })
        .collect();
    if !skipped.is_empty() {
        let _ = writeln!(out, "\nSkipped ({}):", skipped.len());
        for (pkg, reason) in skipped {
            let _ = writeln!(out, "  {pkg}: {reason}");
        }
    }

    // What the fix loop changed in the packages it built or gave up on: the same
    // lines the changelog and the submit request carry.
    let touched = touched_packages(&latest);
    let flagged = fix_flagged(state_root, touched.iter().copied());
    let _ = writeln!(out, "\nFlagged by the fix loop ({}):", flagged.len());
    for (pkg, lines) in &flagged {
        let _ = writeln!(out, "  {pkg}");
        for line in lines {
            let _ = writeln!(out, "    {line}");
        }
    }

    // What the change adds that needs a review before it is submitted.
    let risks = with_lines(touched.iter().copied(), |pkg| {
        ap_build::risk_flags::lines_of_package(state_root, pkg)
    });
    let _ = writeln!(out, "\nNeeds review ({}):", risks.len());
    for (pkg, lines) in &risks {
        let _ = writeln!(out, "  {pkg}");
        for line in lines {
            let _ = writeln!(out, "    {line}");
        }
    }

    let lint = rpmlint_errors_of(state_root, &built);
    if !lint.is_empty() {
        let _ = writeln!(out, "\nrpmlint errors in the final RPMs ({}):", lint.len());
        for line in &lint {
            let _ = writeln!(out, "  {line}");
        }
    }

    // A spec's Version: that could not be checked against its tag: OBS may
    // build the package under another version.
    let mut unchecked: std::collections::BTreeMap<String, String> = Default::default();
    for ev in memory.version_unchecked() {
        if let Event::VersionUnchecked { pkg, reason } = ev {
            unchecked.insert(pkg, reason);
        }
    }
    if !unchecked.is_empty() {
        let _ = writeln!(out, "\nVersion not checked ({}):", unchecked.len());
        for (pkg, reason) in &unchecked {
            let _ = writeln!(out, "  {pkg}: {reason}");
        }
    }

    // OBS reports names in lower case, and this run's packages are already in
    // the built list above, so they are not repeated here.
    let statuses = ap_build::osc::get_project_build_statuses(project, distro, arch)
        // Only the "already built" extras come from this listing; without it the
        // summary still lists everything this run built.
        .unwrap_or_default();
    let built_lower: Vec<String> = built.iter().map(|p| p.to_lowercase()).collect();
    let mut cached: Vec<&String> = statuses
        .iter()
        .filter(|(name, s)| {
            matches!(s, ap_build::osc::BuildStatus::Succeeded)
                && !built_lower.contains(&name.to_lowercase())
        })
        .map(|(name, _)| name)
        .collect();
    cached.sort();
    let _ = writeln!(out, "\nAlready built in {project} ({}):", cached.len());
    for name in cached {
        let _ = writeln!(out, "  {name}");
    }
    out.push_str(&crate::allow_host::refused_section(
        &ap_build::allowed_hosts::shared(),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(
        outcomes: &[(&str, PackageOutcome)],
    ) -> std::collections::BTreeMap<String, (String, PackageOutcome)> {
        outcomes
            .iter()
            .map(|(pkg, o)| (pkg.to_string(), ("1.0".to_string(), o.clone())))
            .collect()
    }

    #[test]
    fn a_package_waiting_on_a_needs_maintainer_stop_reads_failed() {
        let latest = latest(&[
            (
                "gamma",
                PackageOutcome::NeedsMaintainer {
                    reason: "key".into(),
                },
            ),
            ("beta", PackageOutcome::Built),
        ]);
        let failed = failed_packages(&latest);
        assert_eq!(
            waiting_list(&["gamma".to_string(), "beta".to_string()], &failed),
            "failed gamma, beta"
        );
    }

    #[test]
    fn the_fix_loop_flags_of_a_needs_maintainer_stop_are_listed() {
        let latest = latest(&[(
            "gamma",
            PackageOutcome::NeedsMaintainer {
                reason: "key".into(),
            },
        )]);
        assert_eq!(touched_packages(&latest), vec![&"gamma".to_string()]);
    }

    #[test]
    fn an_excluded_build_is_a_skip_not_a_failure() {
        let latest = latest(&[(
            "lsvpd",
            PackageOutcome::Skipped {
                reason: "excluded for x86_64".into(),
            },
        )]);
        assert!(failed_packages(&latest).is_empty());
        assert!(touched_packages(&latest).is_empty());
    }
}
