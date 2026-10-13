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
    NotRecognized {
        source: String,
    },
    LookupFailed {
        ecosystem_tag: String,
        name: String,
        error: String,
    },
    /// Both versions were fetched but at least one is not valid PEP 440, so
    /// the comparator cannot decide which is newer. Previously this fell
    /// into `UpToDate`, which labelled a package `[current]` when the tool
    /// had actually failed to make the comparison at all — the Oct 6
    /// review's probe with a packaged `Version: %{package_version}` that
    /// never got expanded reproduced exactly this false-current label.
    VersionCompareFailed {
        packaged: String,
        latest: String,
    },
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
            SkipReason::LookupFailed {
                ecosystem_tag,
                name,
                error,
            } => {
                write!(f, "{ecosystem_tag} lookup for {name} failed: {error}")
            }
            SkipReason::VersionCompareFailed { packaged, latest } => {
                write!(
                    f,
                    "cannot compare versions (packaged {packaged:?} vs latest {latest:?} — \
                     one or both are not PEP 440); report as unknown rather than UpToDate"
                )
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

/// Return a canonical `https://<host>/<project_path>` for a git-forge URL
/// found inside a `Source:` line, or `None` if the line doesn't point at a
/// recognised git host.
///
/// A spec's `Source:` for a GitHub release often points at an archive
/// (`.../archive/<tag>.tar.gz`); the release-lookup API wants the plain repo
/// URL. SSH URLs (`git@github.com:owner/repo.git`) are also normalised to
/// their HTTPS form. Nested GitLab groups (`group/subgroup/project`) are
/// preserved as-is rather than truncated to two segments.
///
/// The actual parsing lives in [`ap_ecosystem::forge::parse`] so the three
/// forge-URL readers in this codebase stay in sync.
fn git_repo_url(source: &str) -> Option<String> {
    // A `Source:` line may embed the forge URL inside something else (e.g.
    // `Source0: %{url}/archive/v%{version}.tar.gz`), so scan for a known host
    // marker and parse the slice starting at `https://`/`http://`/`git@`.
    let candidates = extract_url_like(source);
    for candidate in candidates {
        if let Some(forge) = ap_ecosystem::forge::parse(&candidate)
            && ap_ecosystem::forge::is_known_host(&forge.host)
        {
            return Some(forge.canonical_https());
        }
    }
    None
}

/// Pull URL-ish substrings out of a `Source:` line.
///
/// A Source line in a spec typically starts with the URL, but macros like
/// `%{url}` and arbitrary file names can appear around it, so this collects
/// every candidate that begins with a scheme we recognise.
fn extract_url_like(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in ["https://", "http://", "ssh://", "git@"] {
        let mut rest = source;
        while let Some(idx) = rest.find(prefix) {
            let tail = &rest[idx..];
            // A URL ends at whitespace or a quote.
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(tail.len());
            out.push(tail[..end].to_string());
            rest = &tail[end..];
        }
    }
    out
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
        Ecosystem::Pypi { pypi_name } => {
            (lookups.fetch_pypi_latest)(pypi_name).map_err(|e| SkipReason::LookupFailed {
                ecosystem_tag: "PyPI".into(),
                name: pypi_name.clone(),
                error: e,
            })
        }
        Ecosystem::GitRepo { url } => {
            (lookups.fetch_repo_latest)(url).map_err(|e| SkipReason::LookupFailed {
                ecosystem_tag: "git".into(),
                name: url.clone(),
                error: e,
            })
        }
    };
    let latest = match latest {
        Ok(v) => v,
        Err(reason) => return skipped(rpm_name, reason),
    };

    let decision = match pypi::compare_pep440(&latest, &header.version) {
        pypi::VersionCompare::Newer => Decision::Update {
            ecosystem,
            old_version: header.version,
            new_version: latest,
        },
        pypi::VersionCompare::NotNewer => Decision::UpToDate {
            ecosystem,
            version: header.version,
        },
        pypi::VersionCompare::Unparseable => Decision::Skipped(SkipReason::VersionCompareFailed {
            packaged: header.version,
            latest,
        }),
    };
    Candidate {
        rpm_name: rpm_name.to_string(),
        decision,
    }
}

fn skipped(rpm_name: &str, reason: SkipReason) -> Candidate {
    Candidate {
        rpm_name: rpm_name.to_string(),
        decision: Decision::Skipped(reason),
    }
}

/// Classify every package in `project` (optionally filtered to just `only`).
///
/// Hits OBS twice per package (`osc ls` to find the real spec filename, then
/// `osc cat <pkg> <picked>.spec`), plus one PyPI call per PyPI-sourced
/// package and one git-forge API call per repo-sourced one. Returned in
/// sorted RPM-name order so `--check` output is stable across runs.
pub fn plan(project: &str, only: Option<&str>, emit: Emit<'_>) -> Vec<Candidate> {
    // A failed listing is already reported by osc; it plans nothing.
    let packages = osc::list_project_packages(project, emit).unwrap_or_default();
    let mut names: Vec<String> = packages
        .into_iter()
        .filter(|n| only.is_none_or(|o| o == n.as_str()))
        .collect();
    names.sort();

    // Enumerate the package's files instead of guessing `<pkg>.spec`.
    // AppCo's versioned-library convention gives one OBS package per
    // upstream major version (`pytorch+2.4`, `pytorch+2.5`), each holding a
    // plain `<base-name>.spec` (`pytorch.spec`) that has no relation to the
    // OBS package name. The old guess silently missed every one of these
    // (Oct 6 review finding #3): `demo+1.0` looked for `demo+1.0.spec`,
    // not the actual `demo.spec`.
    let fetch_spec = |rpm_name: &str| spec_for_package(project, rpm_name, emit);
    let fetch_pypi_latest =
        |pypi_name: &str| pypi::fetch_latest_version(pypi_name).map_err(|e| e.to_string());
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

/// Fetch the primary spec from an OBS package, picking the right filename
/// out of whatever `osc ls` returns.
fn spec_for_package(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    let files = osc::list_package_files(project, pkg, emit)?;
    let picked = pick_spec_filename(pkg, &files)?;
    osc::cat_file(project, pkg, &picked, emit)
}

/// Pure, testable half of [`spec_for_package`]'s filename picker.
///
/// Preference order:
/// 1. `<base>.spec`, where `<base>` is the OBS package name with any
///    `+<version>` suffix stripped (AppCo convention: `pytorch+2.4` →
///    `pytorch.spec`).
/// 2. `<pkg>.spec` verbatim (the pre-Oct 6 default, for the common
///    non-AppCo case).
/// 3. Alphabetical first, so multibuild flavors or alternative spec names
///    still produce a stable choice across runs.
///
/// Returns `None` only when no `.spec` file is present at all.
fn pick_spec_filename(pkg: &str, files: &[String]) -> Option<String> {
    let mut specs: Vec<&String> = files.iter().filter(|f| f.ends_with(".spec")).collect();
    if specs.is_empty() {
        return None;
    }
    specs.sort();
    let bare_spec = format!(
        "{}.spec",
        pkg.split_once('+').map(|(n, _)| n).unwrap_or(pkg),
    );
    let pkg_spec = format!("{pkg}.spec");
    if let Some(s) = specs.iter().find(|s| ***s == bare_spec) {
        return Some((*s).clone());
    }
    if let Some(s) = specs.iter().find(|s| ***s == pkg_spec) {
        return Some((*s).clone());
    }
    specs.first().map(|s| (*s).clone())
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
        let p: &'static dyn Fn(&str) -> Result<String, String> =
            Box::leak(Box::new(move |_name: &str| Ok(latest.to_string())));
        let r: &'static dyn Fn(&str) -> Result<String, String> =
            Box::leak(Box::new(move |_url: &str| -> Result<String, String> {
                unreachable!("repo path not used")
            }));
        Lookups {
            fetch_pypi_latest: p,
            fetch_repo_latest: r,
        }
    }

    fn only_repo(latest: &'static str) -> Lookups<'static> {
        let p: &'static dyn Fn(&str) -> Result<String, String> =
            Box::leak(Box::new(move |_name: &str| -> Result<String, String> {
                unreachable!("pypi path not used")
            }));
        let r: &'static dyn Fn(&str) -> Result<String, String> =
            Box::leak(Box::new(move |_url: &str| Ok(latest.to_string())));
        Lookups {
            fetch_pypi_latest: p,
            fetch_repo_latest: r,
        }
    }

    #[test]
    fn flags_a_newer_upstream_release_for_update() {
        let spec = |_: &str| Some(ATTRS_SPEC.to_string());
        let c = classify("python-attrs", &spec, &only_pypi("27.0.0"));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Pypi {
                    pypi_name: "attrs".into()
                },
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
                ecosystem: Ecosystem::Pypi {
                    pypi_name: "attrs".into()
                },
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
                ecosystem: Ecosystem::Pypi {
                    pypi_name: "nvidia_cublas".into()
                },
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
                ecosystem: Ecosystem::Pypi {
                    pypi_name: "torchaudio".into()
                },
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
        let p: &'static dyn Fn(&str) -> Result<String, String> =
            Box::leak(Box::new(|_: &str| Err("404 Not Found".to_string())));
        let r: &'static dyn Fn(&str) -> Result<String, String> =
            Box::leak(Box::new(|_: &str| -> Result<String, String> {
                unreachable!()
            }));
        let lookups = Lookups {
            fetch_pypi_latest: p,
            fetch_repo_latest: r,
        };
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
    /// AppCo versioned-library packages are named `<base>+<version>` on OBS
    /// but hold a plain `<base>.spec`. The pre-Oct-6 picker guessed
    /// `<pkg>.spec` and silently missed the real one on every such package.
    #[test]
    fn picks_the_bare_name_spec_for_an_appco_versioned_package() {
        let files = vec![
            "_service".to_string(),
            "pytorch.spec".to_string(),
            "pytorch.changes".to_string(),
        ];
        assert_eq!(
            pick_spec_filename("pytorch+2.4", &files).as_deref(),
            Some("pytorch.spec"),
        );
    }

    /// For the common `python-foo` case the file matching `<pkg>.spec` must
    /// still win over anything alphabetically earlier (e.g. a `_service.spec`
    /// placeholder or a related subpackage's spec).
    #[test]
    fn picks_the_pkg_name_spec_when_present_for_the_ordinary_case() {
        let files = vec![
            "_other.spec".to_string(),
            "python-attrs.spec".to_string(),
            "python-attrs.changes".to_string(),
        ];
        assert_eq!(
            pick_spec_filename("python-attrs", &files).as_deref(),
            Some("python-attrs.spec"),
        );
    }

    #[test]
    fn picks_alphabetically_first_spec_when_neither_canonical_name_is_present() {
        let files = vec![
            "zzz.spec".to_string(),
            "aaa.spec".to_string(),
            "mmm.spec".to_string(),
        ];
        assert_eq!(
            pick_spec_filename("something-weird+1", &files).as_deref(),
            Some("aaa.spec"),
        );
    }

    #[test]
    fn no_spec_file_at_all_returns_none() {
        let files = vec!["_service".to_string(), "README.md".to_string()];
        assert_eq!(pick_spec_filename("whatever", &files), None);
    }

    /// A version that is neither obviously newer nor obviously the same —
    /// because one side cannot be parsed as PEP 440 at all — must NOT be
    /// labelled `UpToDate`. The Oct 6 review reproduced the false-current
    /// label with a spec whose `Version: %{package_version}` never
    /// expanded; the comparison was between the literal macro and a real
    /// PyPI version, so parsing failed, so the pre-Oct-6 code concluded
    /// "not newer, therefore current".
    #[test]
    fn an_unparseable_version_is_not_reported_as_up_to_date() {
        let spec = "\
Name:           python-weird
Version:        not-a-valid-pep-440
Source:         https://files.pythonhosted.org/packages/source/w/weird/weird-%{version}.tar.gz
";
        let spec_fn = |_: &str| Some(spec.to_string());
        let c = classify("python-weird", &spec_fn, &only_pypi("2.0.0"));
        match c.decision {
            Decision::Skipped(SkipReason::VersionCompareFailed { packaged, latest }) => {
                assert_eq!(packaged, "not-a-valid-pep-440");
                assert_eq!(latest, "2.0.0");
            }
            other => panic!(
                "expected Skipped(VersionCompareFailed), got {other:?} — this is the \
                 Oct 6 false-UpToDate regression"
            ),
        }
    }

    /// A spec using an AppCo `Version: %{package_version}` pattern should
    /// resolve against the matching `%define`/`%global` earlier in the
    /// file, so the comparison sees the real version. Without this the
    /// version string reaching the comparator is literally
    /// `%{package_version}`, which triggers the VersionCompareFailed path
    /// above (which is at least not-silent, but still blocks genuine
    /// updates).
    #[test]
    fn version_macro_is_expanded_from_a_preceding_define() {
        let spec = "\
%define package_version 2.4.0
Name:           pytorch
Version:        %{package_version}
Source:         https://files.pythonhosted.org/packages/source/p/pytorch/pytorch-%{version}.tar.gz
";
        let spec_fn = |_: &str| Some(spec.to_string());
        let c = classify("pytorch+2.4", &spec_fn, &only_pypi("2.5.0"));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Pypi {
                    pypi_name: "pytorch".into()
                },
                old_version: "2.4.0".into(),
                new_version: "2.5.0".into(),
            },
            "expected the define-resolved 2.4.0 to compare against 2.5.0"
        );
    }

    #[test]
    fn git_repo_url_preserves_nested_gitlab_groups() {
        // A nested GitLab group has to survive intact: GitLab's REST API
        // returns 404 for a truncated path, and the archive URL built from
        // it just doesn't exist. Three-segment path, three-segment URL.
        assert_eq!(
            git_repo_url(
                "https://gitlab.com/group/subgroup/project/-/archive/v1.0/project-v1.0.tar.gz",
            )
            .as_deref(),
            Some("https://gitlab.com/group/subgroup/project"),
        );
    }

    #[test]
    fn git_repo_url_normalises_ssh_to_https() {
        // A `Source:` line pointing at an SSH clone URL was passed through
        // unchanged — the subsequent HTTPS release-API call then couldn't
        // reach it. Now the SSH spelling is reduced to its canonical HTTPS
        // form before anything else runs.
        assert_eq!(
            git_repo_url("git@github.com:NVIDIA/cuda-python.git").as_deref(),
            Some("https://github.com/NVIDIA/cuda-python"),
        );
    }
}
