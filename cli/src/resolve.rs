//! Turning the package name a user gives into the names to look up: the URL and
//! repository forms, the name checks, the upstream candidates, and the dependency
//! configuration of a run.

use super::*;

/// Whether a positional target is a repository URL rather than a package name.
pub(super) fn looks_like_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://") || s.starts_with("git@")
}

/// The package name implied by a repository URL: its repository name.
///
/// `https://github.com/BurntSushi/ripgrep.git` → `ripgrep`; sub-paths
/// (`…/tree/main`), queries and fragments do not leak into the name.
pub(super) fn repo_package_name(url: &str) -> String {
    // A direct tarball URL names the project in its file name, not as the last
    // path segment (`…/hello-2.12.tar.gz` is `hello`, not the file).
    if ap_build::sources::source_kind(url) == ap_build::sources::SourceKind::DirectArchive
        && let Some((name, _)) = url
            .split(['?', '#'])
            .next()
            .and_then(|u| u.trim_end_matches('/').rsplit('/').next())
            .and_then(ap_build::sources::archive_name_version)
    {
        return name;
    }
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(forge) = ap_ecosystem::forge::parse(bare)
        && !forge.repo_name.is_empty()
    {
        return forge.repo_name;
    }
    bare.trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(bare)
        .to_string()
}

/// Reject package names that would be unsafe or meaningless downstream (they
/// flow into osc argv, state paths and pip): empty, leading `-`, or containing
/// `/`, whitespace or control characters.
pub(super) fn validate_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("package name is empty".to_string());
    }
    if name.starts_with('-') {
        return Err(format!(
            "invalid package name '{name}': must not start with '-'"
        ));
    }
    if name.contains('/') || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "invalid package name '{name}': must not contain '/', whitespace or control characters"
        ));
    }
    Ok(())
}

/// Produce candidate upstream-project names for a dep the agent (or OBS)
/// reported in RPM BuildRequires shape, from most-specific to most-stripped.
///
/// The agent is instructed to translate shapes like `libtrantor-devel` or
/// `yaml-cpp-devel` into the upstream project name (`trantor`, `yaml-cpp`)
/// before reporting them. In practice some turns don't do that — the dep
/// then arrives at the source-repo discovery step with the RPM cruft
/// still attached, which doesn't match any real upstream project name
/// and `find_source_repo_candidates` returns no verified candidates.
///
/// We try the original name first (unchanged behavior for correctly-
/// translated deps), then progressively strip the common RPM-library
/// decorations:
///   - `-devel` / `-dev` / `-headers` suffix → the development
///     subpackage of a shared library;
///   - `lib` prefix → the sonamed library package itself.
///
/// Returns the list in try-order, deduped, with the original always
/// first so a one-shot `is_verified` check on `candidates[0]` keeps the
/// pre-fix behavior for already-canonical deps.
pub(super) fn upstream_name_candidates(dep: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![dep.to_string()];
    let suffixes = ["-devel", "-dev", "-headers"];
    let mut stripped = dep.to_string();
    for s in &suffixes {
        if let Some(rest) = stripped.strip_suffix(s) {
            stripped = rest.to_string();
            break;
        }
    }
    if stripped != dep && !out.contains(&stripped) {
        out.push(stripped.clone());
    }
    if let Some(rest) = stripped.strip_prefix("lib")
        && !rest.is_empty()
    {
        let rest = rest.to_string();
        if !out.contains(&rest) {
            out.push(rest);
        }
    }
    out
}

/// PyPI's latest version of `name`, or `None` when PyPI has no such project.
/// Any other failure (network, 5xx, unusable answer) also yields `None`, so
/// the caller falls back to repository discovery, but is warned about first:
/// "PyPI could not be asked" must not read as "not on PyPI".
pub(super) fn pypi_latest_version(name: &str, reporter: &dyn Reporter) -> Option<String> {
    match ap_ecosystem::pypi::fetch_latest_version(name) {
        Ok(v) => Some(v),
        Err(e) if e.is_not_found() => None,
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: format!("PyPI lookup for {name} failed, treating it as not on PyPI: {e}"),
            });
            None
        }
    }
}

/// Resolve a bare dependency name (already stripped of any `python-` RPM
/// prefix) discovered at runtime into a buildable [`PipelineConfig`].
///
/// For Python parents: tries PyPI first — the overwhelmingly common case —
/// then falls back to discovering a public source repository the same way a
/// wheel-only PyPI release does (`find_source_repo_candidates`).
///
/// For non-Python parents (plain C/C++, Rust): skips PyPI and goes straight to
/// source-repo discovery. A C++ library can share its name with an unrelated
/// PyPI package, and PyPI first would queue that package instead of the C++
/// library the parent actually links against. The parent's
/// ecosystem is read off `config.rpm_name`'s `python-` prefix, which the
/// upstream probe sets through `Ecosystem::rpm_name` (see `build/src/sources.rs`).
///
/// Returns `None` — after reporting a `Warning` event explaining why — when
/// neither source has anything. A closed-source/proprietary dependency
/// (NVIDIA's CUDA toolkit `cuda-devel`/`cuda-runtime` are the running
/// example convention 27 already names) has no public source of any kind and
/// must already be provided by the target OBS project's own repository
/// configuration; this tool cannot manufacture one no matter how many times
/// the agent reports it.
pub(super) fn resolve_dependency_config(
    dep: &str,
    native: bool,
    config: &PipelineConfig,
    reporter: &dyn Reporter,
) -> Option<PipelineConfig> {
    // A C/C++ library (reported as `lib<x>-devel`, `pkgconfig(x)`, a soname) is
    // never looked up on PyPI: a PyPI project that happens to share its name is
    // not it, and would be packaged as `python-<x>`.
    let parent_is_python = config.rpm_name.starts_with("python-") && !native;

    // `--nvidia-redist`: a CUDA component is NVIDIA's archive at the version of
    // NVIDIA's manifest, whatever PyPI says about the wheel or a repository
    // about the name.
    if let Some(version) = ap_build::redist::latest_version(&ap_build::redist::Network, dep) {
        return match version {
            Ok(version) => {
                let rpm_name = ap_build::osc::rpm_name(dep);
                ap_core::diag!("  queuing missing dependency: {rpm_name} (NVIDIA {version})");
                Some(PipelineConfig {
                    pkg_name: dep.to_string(),
                    rpm_name,
                    version,
                    source_repo: None,
                    subprojects: Vec::new(),
                    // The root package's findings are not a dependency's.
                    patch_request: None,
                    patch_condition: None,
                    ..config.clone()
                })
            }
            Err(e) => {
                reporter.report(&Event::Warning {
                    step: None,
                    message: format!("cannot package {dep} from NVIDIA's archives: {e}"),
                });
                None
            }
        };
    }

    if parent_is_python {
        // Use the version-only path, not fetch_latest_release: a wheel-only
        // release (NVIDIA CUDA toolkit packages, Triton, etc.) has a real
        // info.version but no sdist, and fetch_latest_release would reject
        // it on the sdist check — leaving the dependency unqueued even
        // though PyPI could tell us its version perfectly well.
        let pypi_version = pypi_latest_version(dep, reporter);

        if let Some(version) = pypi_version {
            ap_core::diag!(
                "  queuing missing dependency: {} (PyPI: {dep})",
                ap_build::osc::rpm_name(dep)
            );
            return Some(PipelineConfig {
                pkg_name: dep.to_string(),
                rpm_name: ap_build::osc::rpm_name(dep),
                version,
                // A PyPI-resolved dependency never comes from a repository, even
                // if the parent package the agent found it from did.
                source_repo: None,
                subprojects: Vec::new(),
                // The root package's findings are not a dependency's.
                patch_request: None,
                patch_condition: None,
                ..config.clone()
            });
        }
    }

    // Non-Python parent, or Python parent whose dep has no PyPI release:
    // discover a public source repository and probe it. The probe decides
    // whether it builds under Python/Rust/PlainC — one more layer of
    // insurance that the queued dep carries the right RPM name regardless of
    // what the parent's ecosystem was.
    //
    // Try candidate upstream-project spellings in sequence: the agent is
    // supposed to translate OBS's BuildRequires-shape name
    // (`libtrantor-devel`) into the upstream project name (`trantor`)
    // before reporting, but doesn't always, and a partially-translated name
    // (`trantor-devel`) would not match `find_source_repo_candidates`.
    // Canonicalize mechanically so a partially-translated dep
    // still has a chance.
    //
    // For EACH candidate name, try EACH verified repo URL — the search
    // can return several same-named repos, and the first one may be a
    // dead fork with no tags. Only give up once no verified URL for any
    // candidate name yields a usable release.
    let mut all_attempts: Vec<String> = Vec::new();
    let mut probe_failure_warnings: Vec<String> = Vec::new();
    let name_candidates = upstream_name_candidates(dep);
    // A repository's latest release does not depend on the name it was found
    // under, so each is asked for once per run, and the new ones for one
    // candidate are asked for side by side.
    let mut releases: std::collections::HashMap<String, Result<String, String>> =
        std::collections::HashMap::new();
    for cand in &name_candidates {
        let repos = ap_build::pipeline::find_source_repo_candidates(cand);
        let unknown: Vec<String> = repos
            .verified
            .iter()
            .filter(|r| !releases.contains_key(*r))
            .cloned()
            .collect();
        releases.extend(
            unknown
                .iter()
                .cloned()
                .zip(ap_build::sources::latest_releases(&unknown)),
        );
        for repo_url in &repos.verified {
            all_attempts.push(repo_url.clone());
            let version = match &releases[repo_url] {
                Ok(v) => v.clone(),
                Err(e) => {
                    probe_failure_warnings
                        .push(format!("{repo_url}: latest-release lookup failed: {e}"));
                    continue;
                }
            };
            let probe = match ap_build::sources::probe(repo_url, &version, cand) {
                Ok(p) => p,
                Err(e) => {
                    probe_failure_warnings.push(format!("{repo_url}: probe failed: {e}"));
                    continue;
                }
            };
            // Found a verified repo whose latest release probes cleanly.
            if cand != dep {
                ap_core::diag!(
                    "  canonicalized {dep} → {cand} to find a source \
                     repository ({repo_url}); agent should have \
                     translated this name itself (strip lib/-devel/-dev \
                     cruft)"
                );
            }
            // Mirrors `cmd_build`'s explicit --source-url handling: the
            // user-given name wins for a monorepo (it names one specific
            // subproject), otherwise the repository's own declared name
            // wins over our guess.
            let effective_name = if probe.subprojects.len() > 1 {
                cand.clone()
            } else {
                probe.project_name.clone().unwrap_or_else(|| cand.clone())
            };
            let dep_rpm = probe.ecosystem.rpm_name(&effective_name);
            // A devel-wrapped dep (`libarrow-devel`) canonicalized to its
            // umbrella project (`arrow`) can probe straight back to the
            // package currently being built (pyarrow lives inside the
            // apache/arrow monorepo, so `arrow` resolves to `python-pyarrow`
            // again). Accepting that would overwrite this package's own
            // in-flight version/config with the monorepo's git-tag version,
            // which then fails spec generation and loops forever reselecting
            // the same broken version. Skip and keep trying other
            // candidates/repos instead.
            if ap_build::osc::fuzzy_key(&dep_rpm) == ap_build::osc::fuzzy_key(&config.rpm_name) {
                probe_failure_warnings.push(format!(
                    "{repo_url}: resolved back to this package's own rpm_name \
                     ({dep_rpm}) — likely the umbrella project of a devel-wrapped \
                     dependency name; skipping"
                ));
                continue;
            }
            ap_core::diag!(
                "  queuing missing dependency: {dep_rpm} \
                 (source repository: {repo_url}, not on PyPI)"
            );
            return Some(PipelineConfig {
                pkg_name: effective_name,
                rpm_name: dep_rpm,
                version,
                source_repo: Some(repo_url.clone()),
                subprojects: probe.subprojects,
                // The root package's findings are not a dependency's.
                patch_request: None,
                patch_condition: None,
                ..config.clone()
            });
        }
    }

    // No candidate × repo combination yielded a usable release. Build
    // one warning that names every URL we actually tried and why each
    // failed, so the user can see which URL was tried and why it did not
    // work, instead of a bare 'no source repo found'.
    let detail = if probe_failure_warnings.is_empty() {
        format!(
            "no verified source repository found at all (tried names: {})",
            name_candidates.join(", "),
        )
    } else {
        format!(
            "every verified repository failed to resolve a usable release \
             (tried {} URL(s) across {} name candidate(s)):\n  - {}",
            all_attempts.len(),
            name_candidates.len(),
            probe_failure_warnings.join("\n  - ")
        )
    };
    reporter.report(&Event::Warning {
        step: None,
        message: format!(
            "{dep} has no PyPI package and no usable public source \
             repository. {detail}. If this is a system library or a \
             closed-source redistribution (like NVIDIA's \
             cuda-devel/cuda-runtime), it must already be provided by \
             the target project's own repository configuration; this \
             tool has no way to manufacture one. Otherwise, pass \
             --source-url {dep}=<url> to point it at the upstream repo \
             (a repository found only by a name search is not used \
             without that confirmation)."
        ),
    });
    None
}
