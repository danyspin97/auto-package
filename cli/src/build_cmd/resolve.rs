use super::*;

/// The version to build: the pinned one, or the latest release of the
/// source repository, the name on the command line, or the repository
/// discovered for it. `source_repo` is set when a repository is discovered.
/// A name PyPI does not carry uses the repository `source_urls` records for
/// it before any search; `work_dir` is where a confirmed one is recorded.
pub(super) fn resolve_version(
    name: &str,
    version: Option<&str>,
    source_repo: &mut Option<String>,
    source_urls: &ap_build::sources::SourceMap,
    work_dir: &Path,
    reporter: &Arc<dyn Reporter + Send + Sync>,
) -> color_eyre::Result<String> {
    Ok(match version {
        Some(v) => v.to_string(),
        None => {
            if let Some(repo) = &source_repo {
                let v = ap_build::sources::latest_release(repo)
                    .map_err(|e| color_eyre::eyre::eyre!(e))?;
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!("{repo} latest release is {v}"),
                });
                v
            } else if let Some(v) =
                ap_build::redist::latest_version(&ap_build::redist::Network, name)
            {
                // `--nvidia-redist`: NVIDIA's version of the component.
                v.map_err(|e| color_eyre::eyre::eyre!(e))?
            } else {
                // Probe PyPI first — the overwhelmingly common case — then
                // fall back to public-source-repository discovery for names
                // PyPI does not carry (standalone Rust crates, plain C/C++
                // libraries, or PyPI releases that ship no usable sdist).
                // Mirrors `resolve_dependency_config` so a bare CLI name
                // goes through the same resolution a runtime-discovered
                // dependency does, instead of hard-failing the moment PyPI
                // comes back empty.
                // Version-only probe, not fetch_latest_release: a wheel-only
                // release (common for vendor SDKs) has a legitimate
                // info.version but no sdist, and the full parser would reject
                // it before the repo-fallback path even got a chance to run.
                let pypi_version = pypi_latest_version(name, reporter.as_ref());
                if let Some(v) = pypi_version {
                    v
                } else {
                    let repo_url = match crate::unconfirmed_offer::remembered(source_urls, name) {
                        Some(url) => url,
                        None => crate::unconfirmed_offer::discover(name, work_dir)?,
                    };
                    let repo_url = &repo_url;
                    let v = ap_build::sources::latest_release(repo_url)
                        .map_err(|e| color_eyre::eyre::eyre!(e))?;
                    reporter.report(&Event::Progress {
                        step: None,
                        message: format!(
                            "{name}: not on PyPI, using discovered source \
                             repository {repo_url} (latest release {v})"
                        ),
                    });
                    *source_repo = Some(repo_url.clone());
                    v
                }
            }
        }
    })
}

/// The name, RPM name, subprojects and unknown-build-system flag a repository
/// build is filed under, decided from its probe. A repository names itself:
/// its own name wins over the guess. Not for a monorepo, where the requested
/// name is the source package's, nor for an unknown build system, whose
/// archive names nothing auto-package can read.
pub(super) fn repo_names(
    name: &str,
    probe: &ap_build::sources::RepoProbe,
) -> (String, String, Vec<(String, String)>, bool) {
    let unknown = probe.ecosystem == ap_build::sources::Ecosystem::Unknown;
    let effective_name = if probe.subprojects.len() > 1 || unknown {
        name.to_string()
    } else {
        probe
            .project_name
            .clone()
            .unwrap_or_else(|| name.to_string())
    };
    let rpm = probe.ecosystem.rpm_name(&effective_name);
    (effective_name, rpm, probe.subprojects.clone(), unknown)
}

/// The name and RPM name a build is filed under, and the repository
/// subprojects. A repository names itself: its own name wins over the guess.
///
/// `refusal` is [`ap_build::sources::recipe_refusal`] for this build: an
/// archive with no build system auto-package knows is accepted only when it
/// is `None`.
pub(super) fn resolve_names(
    name: &str,
    version: &str,
    source_repo: &Option<String>,
    refusal: Option<ap_build::sources::Refusal>,
    reporter: &Arc<dyn Reporter + Send + Sync>,
) -> color_eyre::Result<(String, String, Vec<(String, String)>, bool)> {
    Ok(match source_repo {
        None => (
            name.to_string(),
            ap_build::osc::rpm_name(name),
            Vec::new(),
            false,
        ),
        Some(repo) => {
            let probe = ap_build::sources::probe(repo, &version, name, refusal)
                .map_err(|e| color_eyre::eyre::eyre!(e))?;
            let (effective_name, rpm, subprojects, unknown) = repo_names(name, &probe);
            if !effective_name.eq_ignore_ascii_case(name) {
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!(
                        "{repo} calls itself '{effective_name}', not '{name}' — using that"
                    ),
                });
            }
            if probe.subprojects.len() > 1 {
                reporter.report(&Event::Progress {
                    step: None,
                    message: format!(
                        "{repo} holds {} projects: {}",
                        probe.subprojects.len(),
                        probe
                            .subprojects
                            .iter()
                            .map(|(d, _)| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
            (effective_name, rpm, subprojects, unknown)
        }
    })
}

/// Queue the packages OBS already reports as unresolvable, failed or broken in
/// `project`, when `--sweep` asks for it. The root package itself is never
/// queued here (see the comment in the body).
pub(super) fn seed_stuck_deps(
    project: &str,
    distro: &str,
    arch: &str,
    sweep: bool,
    explicit_target: Option<&str>,
    rpm_name: &str,
    pending_deps: &mut Vec<String>,
    stuck_seeds: &mut std::collections::HashSet<String>,
) -> color_eyre::Result<()> {
    if sweep && explicit_target.is_none() {
        // The root target itself must never enter this generic sweep: it
        // goes on to `resolve_dependency_config` just like any other
        // runtime-discovered dependency, which always re-fetches the
        // *latest* PyPI release and clears `source_repo` (see its doc
        // comment) — silently discarding this exact invocation's
        // `--version`/`--source-url`/`--url` and spawning a second,
        // wrongly-configured `run_package` thread racing the correctly
        // configured one `select_target` selects below, against the same
        // checkout and rpm name. OBS showing the root package itself as
        // Failed/Unresolvable/Broken from an earlier interrupted run is
        // exactly the common case this would trigger on.
        let root_key = ap_build::osc::fuzzy_key(&rpm_name);
        let already_stuck: Vec<String> =
            ap_build::osc::get_project_build_statuses(project, distro, arch)?
                .into_iter()
                .filter(|(_, status)| {
                    matches!(
                        status,
                        ap_build::osc::BuildStatus::Unresolvable
                            | ap_build::osc::BuildStatus::Failed
                            | ap_build::osc::BuildStatus::Broken
                    )
                })
                .map(|(pkg, _)| pkg)
                .filter(|pkg| ap_build::osc::fuzzy_key(pkg) != root_key)
                .collect();
        if !already_stuck.is_empty() {
            ap_core::diag!(
                "{} package(s) already unresolvable/failed/broken in {project}, queuing: {}",
                already_stuck.len(),
                already_stuck.join(", ")
            );
        }
        // Keyed by `fuzzy_key`, not the raw stripped name: OBS's real listing
        // and whatever spelling an agent later reports the same dependency
        // under (`pydantic_core` vs. the real `pydantic-core`, say) are not
        // guaranteed to agree on separators — see `osc::fuzzy_key`'s doc
        // comment. An exact-string seed set would silently fail to recognize
        // its own entries.
        stuck_seeds.extend(
            already_stuck
                .iter()
                .map(|pkg| ap_build::osc::fuzzy_key(pkg.strip_prefix("python-").unwrap_or(pkg))),
        );
        pending_deps.extend(already_stuck);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_build::sources::{Ecosystem, RepoProbe};

    fn probe(ecosystem: Ecosystem, project_name: Option<&str>, subprojects: usize) -> RepoProbe {
        RepoProbe {
            ecosystem,
            project_name: project_name.map(str::to_string),
            project_dir: None,
            bin_names: Vec::new(),
            subprojects: (0..subprojects)
                .map(|i| (format!("dir{i}"), format!("sub{i}")))
                .collect(),
            archive_url: "https://example.org/a.tar.gz".into(),
        }
    }

    #[test]
    fn an_unknown_build_system_keeps_the_requested_name_and_sets_the_flag() {
        // Even an archive that declares a name keeps the one asked for.
        let (name, rpm, subprojects, unknown) =
            repo_names("CageBreak", &probe(Ecosystem::Unknown, Some("Other"), 0));
        assert_eq!(name, "CageBreak");
        assert_eq!(rpm, "cagebreak");
        assert!(subprojects.is_empty());
        assert!(unknown);
    }

    #[test]
    fn a_known_build_system_takes_the_declared_name() {
        let (name, rpm, _, unknown) =
            repo_names("cagebreak", &probe(Ecosystem::Python, Some("Other"), 0));
        assert_eq!(name, "Other");
        assert_eq!(rpm, "python-other");
        assert!(!unknown);
    }

    #[test]
    fn a_monorepo_keeps_the_requested_name() {
        let (name, _, subprojects, unknown) = repo_names(
            "cuda-python",
            &probe(Ecosystem::Python, Some("cuda_core"), 2),
        );
        assert_eq!(name, "cuda-python");
        assert_eq!(subprojects.len(), 2);
        assert!(!unknown);
    }
}
