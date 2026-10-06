//! Detection of packages in an OBS project that have a newer upstream release.
//!
//! The sweep is deliberately read-only: it only identifies *which* packages
//! would be bumped and from what to what. Actually bumping them is the job of
//! the ordinary build pipeline — the update command invokes that per
//! candidate once this module has returned the list.
//!
//! The classifier tries several ecosystems in order against each spec's
//! `Source:` line:
//!
//! 1. A direct PyPI sdist URL (`files.pythonhosted.org/packages/source/…`).
//! 2. A PEP 427 wheel filename, as openSUSE's generator uses for packages
//!    PyPI ships as wheels only (NVIDIA's CUDA toolkit components).
//! 3. A bare `<name>-%{version}.tar.gz` filename, treated speculatively as
//!    a PyPI project name (a 404 on PyPI then becomes a per-package skip, not
//!    a hard failure).
//! 4. A git-forge URL (GitHub/GitLab/Gitea), classified via
//!    [`crate::sources::latest_release`].
//!
//! Everything that doesn't match any of these is listed as
//! `SkipReason::NotRecognized` so the user still sees it in the plan output.

use crate::osc;
use crate::proc::Emit;
use crate::spec;
use ap_ecosystem::pypi;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub rpm_name: String,
    pub decision: Decision,
}

/// Which ecosystem a package came from. Carried on both `Update` and
/// `UpToDate` so `cmd_update` knows how to re-invoke the build pipeline — a
/// git-repo update has to pass `--source-url` back in, a PyPI one doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ecosystem {
    /// Resolved via PyPI's JSON API, by project name. Covers direct sdist
    /// URLs, wheel-only packages, and bare `<name>-%{version}.tar.gz` sources
    /// where the name happens to match a PyPI project.
    Pypi { pypi_name: String },
    /// Resolved via a git forge's releases/tags API.
    GitRepo { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Update {
        ecosystem: Ecosystem,
        old_version: String,
        new_version: String,
    },
    UpToDate {
        ecosystem: Ecosystem,
        version: String,
    },
    Skipped(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    SpecFetchFailed,
    SpecHeaderMissing,
    NoSourceLine,
    /// No ecosystem matched the `Source:` line.
    NotRecognized { source: String },
    LookupFailed { ecosystem_tag: String, name: String, error: String },
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::SpecFetchFailed => write!(f, "osc cat failed for the spec"),
            SkipReason::SpecHeaderMissing => write!(f, "spec has no Name:/Version:"),
            SkipReason::NoSourceLine => write!(f, "spec has no Source: line"),
            SkipReason::NotRecognized { source } => {
                write!(f, "no recognised ecosystem for source ({source})")
            }
            SkipReason::LookupFailed { ecosystem_tag, name, error } => {
                write!(f, "{ecosystem_tag} lookup for {name} failed: {error}")
            }
        }
    }
}

/// The two external lookups [`classify`] needs, bundled so test code can
/// supply fakes without wiring up every call site.
pub struct Lookups<'a> {
    pub fetch_pypi_latest: &'a dyn Fn(&str) -> Result<String, String>,
    pub fetch_repo_latest: &'a dyn Fn(&str) -> Result<String, String>,
}

/// Pick the ecosystem a spec's `Source:` line belongs to.
///
/// The order mirrors how often the pattern appears in practice: direct PyPI
/// URL first (the majority of python-* packages), then wheel filenames
/// (NVIDIA's binary distributions), then bare `-%{version}.tar.gz` templates,
/// then git forges.
fn detect_ecosystem(source: &str, spec_version: &str) -> Option<Ecosystem> {
    if let Some(pypi_name) = pypi::pypi_name_from_source_url(source) {
        return Some(Ecosystem::Pypi { pypi_name });
    }
    if let Some((pypi_name, _version)) = pypi::pypi_name_and_version_from_wheel_filename(source) {
        return Some(Ecosystem::Pypi { pypi_name });
    }
    if let Some(pypi_name) = pypi::pypi_name_from_versioned_tarball(source, spec_version) {
        return Some(Ecosystem::Pypi { pypi_name });
    }
    if let Some(url) = git_repo_url(source) {
        return Some(Ecosystem::GitRepo { url });
    }
    None
}

/// Return a canonical `https://<host>/<owner>/<repo>` for a git-forge URL
/// found inside a `Source:` line, or `None` if the line doesn't point at a
/// recognised git host.
///
/// A spec's `Source:` for a GitHub release often points at an archive
/// (`.../archive/<tag>.tar.gz`); the release-lookup API wants the plain repo
/// URL, so this trims whatever comes after `<owner>/<repo>`.
fn git_repo_url(source: &str) -> Option<String> {
    const HOSTS: &[&str] = &["github.com", "gitlab.com", "codeberg.org"];
    for host in HOSTS {
        if let Some(idx) = source.find(host) {
            let after = &source[idx + host.len()..];
            let after = after.strip_prefix('/')?;
            let parts: Vec<&str> = after.splitn(3, '/').collect();
            if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                let repo = parts[1].trim_end_matches(".git");
                return Some(format!("https://{host}/{}/{repo}", parts[0]));
            }
        }
    }
    None
}

/// Fetch and classify one package. Pure given the injected lookups —
/// factored out of [`plan`] so it is directly testable without talking to
/// OBS, PyPI, or a git forge.
pub fn classify<SpecLookup>(
    rpm_name: &str,
    fetch_spec: &SpecLookup,
    lookups: &Lookups<'_>,
) -> Candidate
where
    SpecLookup: Fn(&str) -> Option<String>,
{
    let Some(spec_text) = fetch_spec(rpm_name) else {
        return skipped(rpm_name, SkipReason::SpecFetchFailed);
    };
    let Some(header) = spec::parse_header(&spec_text) else {
        return skipped(rpm_name, SkipReason::SpecHeaderMissing);
    };
    let Some(source) = header.source else {
        return skipped(rpm_name, SkipReason::NoSourceLine);
    };
    let Some(ecosystem) = detect_ecosystem(&source, &header.version) else {
        return skipped(rpm_name, SkipReason::NotRecognized { source });
    };

    let latest = match &ecosystem {
        Ecosystem::Pypi { pypi_name } => (lookups.fetch_pypi_latest)(pypi_name).map_err(|e| {
            SkipReason::LookupFailed {
                ecosystem_tag: "PyPI".into(),
                name: pypi_name.clone(),
                error: e,
            }
        }),
        Ecosystem::GitRepo { url } => (lookups.fetch_repo_latest)(url).map_err(|e| {
            SkipReason::LookupFailed {
                ecosystem_tag: "git".into(),
                name: url.clone(),
                error: e,
            }
        }),
    };
    let latest = match latest {
        Ok(v) => v,
        Err(reason) => return skipped(rpm_name, reason),
    };

    let decision = if pypi::is_newer_pep440(&latest, &header.version) {
        Decision::Update {
            ecosystem,
            old_version: header.version,
            new_version: latest,
        }
    } else {
        Decision::UpToDate {
            ecosystem,
            version: header.version,
        }
    };
    Candidate { rpm_name: rpm_name.to_string(), decision }
}

fn skipped(rpm_name: &str, reason: SkipReason) -> Candidate {
    Candidate { rpm_name: rpm_name.to_string(), decision: Decision::Skipped(reason) }
}

/// Classify every package in `project` (optionally filtered to just `only`).
///
/// Hits OBS twice per package (`osc ls`, then `osc cat <pkg> <pkg>.spec`),
/// plus one PyPI call per PyPI-sourced package and one git-forge API call
/// per repo-sourced one. Returned in sorted RPM-name order so `--check`
/// output is stable across runs.
pub fn plan(project: &str, only: Option<&str>, emit: Emit<'_>) -> Vec<Candidate> {
    let packages = osc::list_project_packages(project, emit);
    let mut names: Vec<String> = packages
        .into_iter()
        .filter(|n| only.is_none_or(|o| o == n))
        .collect();
    names.sort();

    let fetch_spec =
        |rpm_name: &str| osc::cat_file(project, rpm_name, &format!("{rpm_name}.spec"), emit);
    let fetch_pypi_latest = |pypi_name: &str| {
        pypi::fetch_latest_version(pypi_name).map_err(|e| e.to_string())
    };
    let fetch_repo_latest = |url: &str| crate::sources::latest_release(url);
    let lookups = Lookups {
        fetch_pypi_latest: &fetch_pypi_latest,
        fetch_repo_latest: &fetch_repo_latest,
    };

    names
        .iter()
        .map(|n| classify(n, &fetch_spec, &lookups))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTRS_SPEC: &str = "\
Name:           python-attrs
Version:        26.1.0
Source:         https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz
";

    const NVIDIA_CUBLAS_SPEC: &str = "\
Name:           python-nvidia-cublas
Version:        13.1.1.3
Source0:        nvidia_cublas-13.1.1.3-py3-none-manylinux_2_27_x86_64.whl
";

    const TORCHAUDIO_SPEC: &str = "\
Name:           python-torchaudio
Version:        2.4.0
Source:         torchaudio-%{version}.tar.gz
";

    const RIPGREP_SPEC: &str = "\
Name:           ripgrep
Version:        14.1.0
Source:         https://github.com/BurntSushi/ripgrep/archive/14.1.0.tar.gz
";

    fn only_pypi(latest: &'static str) -> Lookups<'static> {
        let p: &'static (dyn Fn(&str) -> Result<String, String>) = Box::leak(Box::new(
            move |_name: &str| Ok(latest.to_string()),
        ));
        let r: &'static (dyn Fn(&str) -> Result<String, String>) = Box::leak(Box::new(
            move |_url: &str| -> Result<String, String> { unreachable!("repo path not used") },
        ));
        Lookups { fetch_pypi_latest: p, fetch_repo_latest: r }
    }

    fn only_repo(latest: &'static str) -> Lookups<'static> {
        let p: &'static (dyn Fn(&str) -> Result<String, String>) = Box::leak(Box::new(
            move |_name: &str| -> Result<String, String> { unreachable!("pypi path not used") },
        ));
        let r: &'static (dyn Fn(&str) -> Result<String, String>) = Box::leak(Box::new(
            move |_url: &str| Ok(latest.to_string()),
        ));
        Lookups { fetch_pypi_latest: p, fetch_repo_latest: r }
    }

    #[test]
    fn flags_a_newer_upstream_release_for_update() {
        let spec = |_: &str| Some(ATTRS_SPEC.to_string());
        let c = classify("python-attrs", &spec, &only_pypi("27.0.0"));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Pypi { pypi_name: "attrs".into() },
                old_version: "26.1.0".into(),
                new_version: "27.0.0".into(),
            }
        );
    }

    #[test]
    fn reports_up_to_date_when_versions_match() {
        let spec = |_: &str| Some(ATTRS_SPEC.to_string());
        let c = classify("python-attrs", &spec, &only_pypi("26.1.0"));
        assert_eq!(
            c.decision,
            Decision::UpToDate {
                ecosystem: Ecosystem::Pypi { pypi_name: "attrs".into() },
                version: "26.1.0".into(),
            }
        );
    }

    #[test]
    fn reports_up_to_date_when_packaged_is_newer_than_pypi() {
        let spec = |_: &str| Some(ATTRS_SPEC.to_string());
        let c = classify("python-attrs", &spec, &only_pypi("25.0.0"));
        assert!(matches!(c.decision, Decision::UpToDate { .. }));
    }

    #[test]
    fn wheel_only_source_resolves_through_pypi() {
        let spec = |_: &str| Some(NVIDIA_CUBLAS_SPEC.to_string());
        let c = classify("python-nvidia-cublas", &spec, &only_pypi("13.2.0.0"));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Pypi { pypi_name: "nvidia_cublas".into() },
                old_version: "13.1.1.3".into(),
                new_version: "13.2.0.0".into(),
            }
        );
    }

    #[test]
    fn bare_tarball_template_resolves_through_pypi_speculatively() {
        let spec = |_: &str| Some(TORCHAUDIO_SPEC.to_string());
        let c = classify("python-torchaudio", &spec, &only_pypi("2.5.0"));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Pypi { pypi_name: "torchaudio".into() },
                old_version: "2.4.0".into(),
                new_version: "2.5.0".into(),
            }
        );
    }

    #[test]
    fn github_source_resolves_through_the_git_forge_api() {
        let spec = |_: &str| Some(RIPGREP_SPEC.to_string());
        let c = classify("ripgrep", &spec, &only_repo("14.2.0"));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::GitRepo {
                    url: "https://github.com/BurntSushi/ripgrep".into(),
                },
                old_version: "14.1.0".into(),
                new_version: "14.2.0".into(),
            }
        );
    }

    #[test]
    fn skips_when_osc_cat_returns_nothing() {
        let spec = |_: &str| None;
        let c = classify("gone", &spec, &only_pypi("x"));
        assert_eq!(c.decision, Decision::Skipped(SkipReason::SpecFetchFailed));
    }

    #[test]
    fn skips_when_pypi_lookup_errors() {
        let spec = |_: &str| Some(ATTRS_SPEC.to_string());
        let p: &'static (dyn Fn(&str) -> Result<String, String>) =
            Box::leak(Box::new(|_: &str| Err("404 Not Found".to_string())));
        let r: &'static (dyn Fn(&str) -> Result<String, String>) = Box::leak(Box::new(
            |_: &str| -> Result<String, String> { unreachable!() },
        ));
        let lookups = Lookups { fetch_pypi_latest: p, fetch_repo_latest: r };
        let c = classify("python-attrs", &spec, &lookups);
        assert_eq!(
            c.decision,
            Decision::Skipped(SkipReason::LookupFailed {
                ecosystem_tag: "PyPI".into(),
                name: "attrs".into(),
                error: "404 Not Found".into(),
            })
        );
    }

    #[test]
    fn skips_when_source_is_unrecognised() {
        let spec_text = "\
Name:           weird
Version:        1.0
Source:         file:///opt/thing.tar.gz
";
        let spec = |_: &str| Some(spec_text.to_string());
        let c = classify("weird", &spec, &only_pypi("x"));
        assert!(matches!(
            c.decision,
            Decision::Skipped(SkipReason::NotRecognized { .. })
        ));
    }

    #[test]
    fn github_archive_url_is_trimmed_to_the_bare_repo() {
        assert_eq!(
            git_repo_url("https://github.com/BurntSushi/ripgrep/archive/14.1.0.tar.gz").as_deref(),
            Some("https://github.com/BurntSushi/ripgrep"),
        );
        assert_eq!(
            git_repo_url("https://github.com/pallets/markupsafe/").as_deref(),
            Some("https://github.com/pallets/markupsafe"),
        );
    }

    #[test]
    fn git_repo_url_rejects_non_forge_hosts() {
        assert_eq!(
            git_repo_url("https://files.pythonhosted.org/packages/source/a/attrs/attrs.tar.gz"),
            None,
        );
    }
}
