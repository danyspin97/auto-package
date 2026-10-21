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
use crate::upstream_cache;
use ap_core::event::Event;
use ap_ecosystem::pypi;
use std::collections::HashMap;
use std::sync::Mutex;

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
    /// Resolved from the release tarballs listed beside the spec's `Source:`
    /// URL (a directory listing, or SourceForge's file feed). `url` is that
    /// source URL, expanded.
    Index { url: String },
    /// Resolved from Repology's record of the project, for a package whose spec
    /// does not say where its source is kept.
    Repology { project: String },
}

impl Ecosystem {
    /// Whether the build pipeline can build a new version found this way. The
    /// others are reported, since the pipeline can neither fetch nor unpack
    /// what they name yet.
    pub fn can_build(&self) -> bool {
        matches!(self, Ecosystem::Pypi { .. } | Ecosystem::GitRepo { .. })
    }

    /// A short name for plan output.
    pub fn tag(&self) -> &'static str {
        match self {
            Ecosystem::Pypi { .. } => "pypi",
            Ecosystem::GitRepo { .. } => "git",
            Ecosystem::Index { .. } => "index",
            Ecosystem::Repology { .. } => "repology",
        }
    }
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
    /// The packaged `Version:` is computed by a shell macro (`%(…)`), so only
    /// the part before it is known, and upstream has the same there.
    ComputedVersion {
        packaged: String,
        known: String,
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
            SkipReason::ComputedVersion {
                packaged,
                known,
                latest,
            } => write!(
                f,
                "the version is computed by a script ({packaged}), so only {known} of it is \
                 known, and upstream's {latest} starts the same"
            ),
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
    /// Newest version in the release listing beside a source URL.
    pub fetch_index_latest: &'a dyn Fn(&str) -> Result<String, String>,
    /// Newest version Repology knows for a project.
    pub fetch_repology_latest: &'a dyn Fn(&str) -> Result<String, String>,
}

/// The places a package's newest version may be looked up, most specific
/// first. A package is checked against each until one answers, so a guess that
/// turns out wrong (a bare tarball name that is not a PyPI project) only moves
/// on to the next. Repology comes last and is always there, which is why a
/// package with an odd or missing `Source:` line is still checked.
///
/// The order mirrors how often the pattern appears in practice: direct PyPI
/// URL first (the majority of python-* packages), then wheel filenames
/// (NVIDIA's binary distributions), then bare `-%{version}.tar.gz` templates,
/// then git forges, then release listings.
fn ecosystems(header: &spec::Header) -> Vec<Ecosystem> {
    let mut out: Vec<Ecosystem> = Vec::new();
    let mut add = |e: Ecosystem| {
        if !out.contains(&e) {
            out.push(e);
        }
    };
    let source = header.source.as_deref().unwrap_or_default();
    if let Some(pypi_name) = pypi::pypi_name_from_source_url(source) {
        add(Ecosystem::Pypi { pypi_name });
    }
    if let Some((pypi_name, _version)) = pypi::pypi_name_and_version_from_wheel_filename(source) {
        add(Ecosystem::Pypi { pypi_name });
    }
    if let Some(pypi_name) = pypi::pypi_name_from_versioned_tarball(source, &header.version) {
        add(Ecosystem::Pypi { pypi_name });
    }
    let expanded = header.expanded_source.as_deref().unwrap_or_default();
    for line in [source, expanded, header.url.as_deref().unwrap_or_default()] {
        if let Some(url) = git_repo_url(line) {
            add(Ecosystem::GitRepo { url });
        }
    }
    if (expanded.starts_with("http://") || expanded.starts_with("https://"))
        && crate::sources::is_any_archive(crate::sources::last_segment(expanded))
    {
        add(Ecosystem::Index {
            url: expanded.to_string(),
        });
    }
    add(Ecosystem::Repology {
        project: header.name.clone(),
    });
    out
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
    // The first place that knows a version decides. When none does, the first
    // failure is the one reported: it belongs to the most specific guess.
    let mut first_failure: Option<SkipReason> = None;
    let mut found = None;
    for ecosystem in ecosystems(&header) {
        let (tag, name, latest) = match &ecosystem {
            Ecosystem::Pypi { pypi_name } => {
                ("PyPI", pypi_name, (lookups.fetch_pypi_latest)(pypi_name))
            }
            Ecosystem::GitRepo { url } => ("git", url, (lookups.fetch_repo_latest)(url)),
            Ecosystem::Index { url } => ("release listing", url, (lookups.fetch_index_latest)(url)),
            Ecosystem::Repology { project } => (
                "Repology",
                project,
                (lookups.fetch_repology_latest)(project),
            ),
        };
        match latest {
            Ok(v) => {
                found = Some((ecosystem, v));
                break;
            }
            Err(error) => {
                first_failure.get_or_insert(SkipReason::LookupFailed {
                    ecosystem_tag: tag.into(),
                    name: name.clone(),
                    error,
                });
            }
        }
    }
    let Some((ecosystem, latest)) = found else {
        return skipped(rpm_name, first_failure.unwrap_or(SkipReason::NoSourceLine));
    };

    let latest = version_part(&latest);
    if let Some(known) = computed_prefix(&header.version) {
        let decision = match newer_than_known(&latest, &known) {
            true => Decision::Update {
                ecosystem,
                old_version: header.version,
                new_version: latest,
            },
            false => Decision::Skipped(SkipReason::ComputedVersion {
                packaged: header.version,
                known,
                latest,
            }),
        };
        return Candidate {
            rpm_name: rpm_name.to_string(),
            decision,
        };
    }
    let decision = match compare_versions(&latest, &header.version) {
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

/// Whether `latest` is newer than `packaged`: by PEP 440 when both are, and by
/// comparing their numeric and alphabetic parts otherwise, as most non-Python
/// projects number their releases.
fn compare_versions(latest: &str, packaged: &str) -> pypi::VersionCompare {
    match pypi::compare_pep440(latest, packaged) {
        // Only versions that look like release numbers: an unexpanded macro or a
        // word as the packaged version must stay unknown, not become "current".
        pypi::VersionCompare::Unparseable if plain_version(latest) && plain_version(packaged) => {
            if crate::sources::version_cmp(latest, packaged).is_gt() {
                pypi::VersionCompare::Newer
            } else {
                pypi::VersionCompare::NotNewer
            }
        }
        decided => decided,
    }
}

/// The release number inside what a lookup returned. A tag may carry a project
/// name that is not the repository's (`accel-config-v4.1.9` in the repository
/// `idxd-config`), which no prefix rule can know; the version is what follows
/// the last separator that is followed by a number, without its `v`.
fn version_part(found: &str) -> String {
    let found = found.trim();
    if plain_version(found) {
        return found.strip_prefix(['v', 'V']).unwrap_or(found).to_string();
    }
    let bytes = found.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if !matches!(b, b'-' | b'_' | b'/') {
            continue;
        }
        let rest = &found[i + 1..];
        let number = rest.strip_prefix(['v', 'V']).unwrap_or(rest);
        if number.starts_with(|c: char| c.is_ascii_digit()) && plain_version(number) {
            return number.to_string();
        }
    }
    found.to_string()
}

/// The known start of a `Version:` that a shell macro finishes
/// (`5.3.%(bash get_version_number.sh)` is `5.3`), when that start is a release
/// number.
fn computed_prefix(version: &str) -> Option<String> {
    let start = version.find("%(")?;
    let known = version[..start].trim_end_matches(['.', '-', '_', '+']);
    plain_version(known).then(|| known.to_string())
}

/// The leading dotted numbers of a version: `5.3p20` is `[5, 3]`.
fn leading_numbers(v: &str) -> Vec<u64> {
    let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
    let end = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    v[..end]
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

/// Whether `latest` is ahead of `known` in the components `known` has.
fn newer_than_known(latest: &str, known: &str) -> bool {
    let known = leading_numbers(known);
    let latest = leading_numbers(latest);
    let n = known.len().min(latest.len());
    n == known.len() && latest[..n] > known[..n]
}

/// A version that starts like a release number (`2.3.1`, `v1.0-rc2`, `20240101`)
/// and holds only the characters one is written with.
fn plain_version(v: &str) -> bool {
    v.strip_prefix(['v', 'V'])
        .unwrap_or(v)
        .starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '~'))
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
/// sorted RPM-name order so `--check` output is stable across runs. Fails
/// when the project's package listing cannot be read.
pub fn plan(project: &str, only: Option<&str>, emit: Emit<'_>) -> Result<Vec<Candidate>, String> {
    plan_where(
        project,
        &|n| only.is_none_or(|o| osc::fuzzy_key(o) == osc::fuzzy_key(n)),
        emit,
    )
}

/// [`plan`] over the packages of `project` that `wanted` accepts. Packages it
/// rejects are not fetched at all, which is what makes a few names out of a
/// large project cheap.
pub fn plan_where(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
) -> Result<Vec<Candidate>, String> {
    let mut candidates = Vec::new();
    plan_each(project, wanted, emit, &mut |c| {
        candidates.push(c);
        true
    })?;
    Ok(candidates)
}

/// [`plan_where`] handing each package to `on` as soon as it is classified,
/// so a caller can start on the first update while the rest are still being
/// checked. `on` returns whether to go on; `false` stops the sweep.
pub fn plan_each(
    project: &str,
    wanted: &dyn Fn(&str) -> bool,
    emit: Emit<'_>,
    on: &mut dyn FnMut(Candidate) -> bool,
) -> Result<(), String> {
    // A failed listing is an error: treating it as "no packages" would make
    // `update` report success for a project it never looked at.
    let packages = osc::try_list_project_packages(project, emit)?;
    let mut names: Vec<String> = packages.iter().filter(|n| wanted(n)).cloned().collect();
    names.sort();

    // Enumerate the package's files instead of guessing `<pkg>.spec`.
    // AppCo's versioned-library convention gives one OBS package per
    // upstream major version (`pytorch+2.4`, `pytorch+2.5`), each holding a
    // plain `<base-name>.spec` (`pytorch.spec`) that has no relation to the
    // OBS package name. The old guess silently missed every one of these
    // (Oct 6 review finding #3): `demo+1.0` looked for `demo+1.0.spec`,
    // not the actual `demo.spec`.
    let fetch_spec = |rpm_name: &str| {
        let spec = spec_for_package(project, rpm_name, emit);
        stash_spec(project, rpm_name, spec.as_ref());
        spec
    };
    let fetch_pypi_latest = |pypi_name: &str| {
        upstream_cache::lookup(&format!("pypi:{pypi_name}"), || {
            pypi::fetch_latest_version(pypi_name).map_err(|e| e.to_string())
        })
    };
    let fetch_repo_latest = |url: &str| {
        upstream_cache::lookup(&format!("repo:{url}"), || {
            crate::sources::latest_release(url)
        })
    };
    let fetch_index_latest = |url: &str| {
        upstream_cache::lookup(&format!("index:{url}"), || {
            crate::upstream::latest_in_index(url)
        })
    };
    let fetch_repology_latest = |name: &str| {
        upstream_cache::lookup(&format!("repology:{name}"), || {
            crate::upstream::latest_on_repology(name)
        })
    };
    let (hits_before, fetched_before) = upstream_cache::stats();
    let lookups = Lookups {
        fetch_pypi_latest: &fetch_pypi_latest,
        fetch_repo_latest: &fetch_repo_latest,
        fetch_index_latest: &fetch_index_latest,
        fetch_repology_latest: &fetch_repology_latest,
    };

    let total = names.len();
    for (i, n) in names.iter().enumerate() {
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: format!("checking {n} ({}/{total})", i + 1),
        });
        if !on(classify(n, &fetch_spec, &lookups)) {
            break;
        }
    }
    let (hits, fetched) = upstream_cache::stats();
    let (hits, fetched) = (hits - hits_before, fetched - fetched_before);
    if hits > 0 {
        emit.reporter.report(&Event::Progress {
            step: emit.step,
            message: format!(
                "upstream versions: {hits} from the cache, {fetched} asked upstream \
                 (--refresh asks them all)"
            ),
        });
    }
    Ok(())
}

/// Specs a plan has just read, by (project, package), for [`planned_spec`].
static PLANNED_SPECS: Mutex<Option<HashMap<(String, String), String>>> = Mutex::new(None);

fn stash_spec(project: &str, pkg: &str, spec: Option<&String>) {
    if let Some(spec) = spec {
        PLANNED_SPECS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert((project.to_string(), pkg.to_string()), spec.clone());
    }
}

/// [`spec_for_package`] for a project that is only read during this run: the
/// spec a plan fetched a moment ago is handed over, once, instead of fetching
/// it again. Taken rather than kept, so a later read, after the package has
/// been changed, always asks OBS.
pub fn planned_spec(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
    let kept = PLANNED_SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|m| m.remove(&(project.to_string(), pkg.to_string())));
    kept.or_else(|| spec_for_package(project, pkg, emit))
}

/// Fetch the primary spec from an OBS package, picking the right filename
/// out of whatever `osc ls` returns.
pub fn spec_for_package(project: &str, pkg: &str, emit: Emit<'_>) -> Option<String> {
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
pub fn pick_spec_filename(pkg: &str, files: &[String]) -> Option<String> {
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

    /// Lookups that find nothing, to fill the ones a test does not exercise.
    fn unused_lookups() -> Lookups<'static> {
        fn none(_: &str) -> Result<String, String> {
            Err("not looked up in this test".to_string())
        }
        Lookups {
            fetch_pypi_latest: &none,
            fetch_repo_latest: &none,
            fetch_index_latest: &none,
            fetch_repology_latest: &none,
        }
    }

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
            ..unused_lookups()
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
            ..unused_lookups()
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
            ..unused_lookups()
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
        // Nothing in the spec says where the source lives, so only Repology
        // is left to ask.
        assert!(matches!(
            c.decision,
            Decision::Skipped(SkipReason::LookupFailed { ref ecosystem_tag, .. })
                if ecosystem_tag == "Repology"
        ));
    }

    fn lookups_for(
        index: &'static dyn Fn(&str) -> Result<String, String>,
        repology: &'static dyn Fn(&str) -> Result<String, String>,
    ) -> Lookups<'static> {
        Lookups {
            fetch_index_latest: index,
            fetch_repology_latest: repology,
            ..unused_lookups()
        }
    }

    #[test]
    fn a_source_url_with_macros_is_expanded_and_its_listing_read() {
        let spec_text = "\
Name:           acl
Version:        2.3.1
Url:            https://savannah.nongnu.org/projects/acl
Source:         https://download.savannah.nongnu.org/releases/acl/%{name}-%{version}.tar.xz#/acl.tar.xz
";
        let spec = |_: &str| Some(spec_text.to_string());
        let index: &'static dyn Fn(&str) -> Result<String, String> = &|url: &str| {
            assert_eq!(
                url,
                "https://download.savannah.nongnu.org/releases/acl/acl-2.3.1.tar.xz"
            );
            Ok("2.3.2".to_string())
        };
        let c = classify("acl", &spec, &lookups_for(index, &|_| Err("no".into())));
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Index {
                    url: "https://download.savannah.nongnu.org/releases/acl/acl-2.3.1.tar.xz"
                        .into()
                },
                old_version: "2.3.1".into(),
                new_version: "2.3.2".into(),
            }
        );
        assert!(
            !matches!(&c.decision, Decision::Update { ecosystem, .. } if ecosystem.can_build())
        );
    }

    #[test]
    fn a_package_without_a_source_line_is_asked_of_repology() {
        let spec_text = "Name: fillup\nVersion: 1.42\n";
        let spec = |_: &str| Some(spec_text.to_string());
        let repology: &'static dyn Fn(&str) -> Result<String, String> = &|name: &str| {
            assert_eq!(name, "fillup");
            Ok("1.43".to_string())
        };
        let c = classify(
            "fillup",
            &spec,
            &lookups_for(&|_| Err("no".into()), repology),
        );
        assert_eq!(
            c.decision,
            Decision::Update {
                ecosystem: Ecosystem::Repology {
                    project: "fillup".into()
                },
                old_version: "1.42".into(),
                new_version: "1.43".into(),
            }
        );
    }

    #[test]
    fn a_wrong_pypi_guess_moves_on_instead_of_skipping() {
        // `dracut-%{version}.tar.xz` looks like a PyPI project name to the
        // name-based guess, which PyPI does not know.
        let spec_text = "Name: dracut\nVersion: 059\nSource: dracut-%{version}.tar.xz\n";
        let spec = |_: &str| Some(spec_text.to_string());
        let pypi: &'static dyn Fn(&str) -> Result<String, String> =
            &|_: &str| Err("404 Not Found".to_string());
        let lookups = Lookups {
            fetch_pypi_latest: pypi,
            fetch_repology_latest: &|_: &str| Ok("060".to_string()),
            ..unused_lookups()
        };
        let c = classify("dracut", &spec, &lookups);
        assert!(
            matches!(c.decision, Decision::Update { ref new_version, .. } if new_version == "060")
        );
    }

    #[test]
    fn a_project_name_in_front_of_a_tag_version_is_dropped() {
        assert_eq!(version_part("accel-config-v4.1.9"), "4.1.9");
        assert_eq!(version_part("release/2.3"), "2.3");
        assert_eq!(version_part("v1.2.3"), "1.2.3");
        assert_eq!(version_part("1.2.3"), "1.2.3");
        assert_eq!(version_part("not-a-version"), "not-a-version");
        assert_eq!(version_part("foo-2.0-rc1"), "2.0-rc1");
    }

    #[test]
    fn a_version_finished_by_a_script_is_compared_on_what_is_known() {
        assert_eq!(
            computed_prefix("5.3.%(bash %{_sourcedir}/get_version_number.sh %{_sourcedir})")
                .as_deref(),
            Some("5.3")
        );
        assert_eq!(computed_prefix("1.2"), None);
        assert_eq!(computed_prefix("%(date)"), None);
        assert!(!newer_than_known("5.3p20", "5.3"));
        assert!(newer_than_known("5.4", "5.3"));
        assert!(newer_than_known("6.0p1", "5.3"));
        assert!(
            !newer_than_known("5", "5.3"),
            "upstream says less than is known"
        );
    }

    #[test]
    fn dotted_numbers_compare_by_part_and_words_stay_unknown() {
        use pypi::VersionCompare::{Newer, NotNewer, Unparseable};
        assert!(matches!(compare_versions("2.10", "2.9"), Newer));
        assert!(matches!(compare_versions("2.9", "2.10"), NotNewer));
        assert!(matches!(compare_versions("059", "059"), NotNewer));
        assert!(matches!(compare_versions("1.0", "%{ver}"), Unparseable));
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
