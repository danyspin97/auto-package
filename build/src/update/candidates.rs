use super::*;
use crate::obs_xml::{mask_xml_comments, open_tag, param_value, service_elements, source_params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub rpm_name: String,
    pub decision: Decision,
    /// What a check found worth a line in the plan, without stopping the
    /// package (a patch-level disagreement with Repology).
    pub notes: Vec<String>,
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
    /// Resolved from rubygems.org's latest-version record of the gem a
    /// `rubygem-` package names. Asked after Repology, when that has no newest.
    Rubygems { gem: String },
    /// Resolved from MetaCPAN's latest release of a CPAN distribution, for a
    /// `perl-` package or one whose Source or Url is on a CPAN host. Asked
    /// after Repology, when that has no newest.
    Cpan { dist: String },
}

impl Ecosystem {
    /// Whether the build pipeline can build a new version found this way. The
    /// others are reported, since the pipeline can neither fetch nor unpack
    /// what they name yet.
    pub fn can_build(&self) -> bool {
        matches!(
            self,
            Ecosystem::Pypi { .. } | Ecosystem::GitRepo { .. } | Ecosystem::Index { .. }
        )
    }

    /// A short name for plan output.
    pub fn tag(&self) -> &'static str {
        match self {
            Ecosystem::Pypi { .. } => "pypi",
            Ecosystem::GitRepo { .. } => "git",
            Ecosystem::Index { .. } => "index",
            Ecosystem::Repology { .. } => "repology",
            Ecosystem::Rubygems { .. } => "rubygems",
            Ecosystem::Cpan { .. } => "cpan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Update {
        ecosystem: Ecosystem,
        old_version: String,
        /// The RPM version to build: the upstream tag's, for a dashed tag.
        new_version: String,
        /// The upstream tag `new_version` stands for, when it is not the
        /// version as it is written (`v2.9.0-1` for `2.9.0.1`).
        upstream_tag: Option<String>,
    },
    UpToDate {
        ecosystem: Ecosystem,
        version: String,
    },
    Skipped(SkipReason),
}

/// Whether a version is a calendar date, the whole of it: `2020-11-13`, or the
/// compact `20201113`. A date inside a longer version (`2.20191203`) is not one.
/// Upstream numbers its releases by day, not by a number of the package's kind.
pub(crate) fn is_dated(version: &str) -> bool {
    let b = version.as_bytes();
    let dashed = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && is_calendar_date(&b[..4], &b[5..7], &b[8..]);
    dashed || (b.len() == 8 && is_calendar_date(&b[..4], &b[4..6], &b[6..]))
}

/// Whether year, month and day are digits that make a date from 1900 to 2099,
/// with a day that the month has (February 29 only in a leap year).
fn is_calendar_date(year: &[u8], month: &[u8], day: &[u8]) -> bool {
    let num = |s: &[u8]| {
        (!s.is_empty() && s.iter().all(u8::is_ascii_digit))
            .then(|| std::str::from_utf8(s).ok()?.parse::<u32>().ok())
            .flatten()
    };
    let (Some(y), Some(m), Some(d)) = (num(year), num(month), num(day)) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1900..=2099).contains(&y) && (1..=days).contains(&d)
}

/// What to do about a failed lookup, from the way it failed.
pub(super) fn lookup_hint(error: &str) -> &'static str {
    let e = error.to_ascii_lowercase();
    if e.contains("404") || e.contains("410") || e.contains("not found") {
        " [the upstream address looks dead; point Source:/URL: at the project's current home]"
    } else if e.contains("certificate") || e.contains("cert verif") {
        " [the host's TLS certificate is not valid; retry later or use another upstream]"
    } else if e.contains("has no newest version") {
        " [Repology knows the project but marks no release as newest; check upstream by hand]"
    } else if e.contains("429") || e.contains("rate limit") {
        " [rate limited; retry later]"
    } else if e.contains("timed out") || e.contains("dns") || e.contains("connection") {
        " [the host could not be reached; retry later]"
    } else {
        ""
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
    /// Latest version rubygems.org lists for a gem.
    pub fetch_gem_latest: &'a dyn Fn(&str) -> Result<String, String>,
    /// Latest release MetaCPAN lists for a CPAN distribution.
    pub fetch_cpan_latest: &'a dyn Fn(&str) -> Result<String, String>,
    /// The package's `_service` text, when it can be read (see
    /// [`service_git_url`]).
    pub fetch_service: &'a dyn Fn(&str) -> Option<String>,
    /// Whether the package holds a `_service` (asked only for a package that
    /// has no source URL, to tell whether something fetches its source).
    pub fetch_has_service: &'a dyn Fn(&str) -> bool,
    /// Newest release of a forge repository, with the day it came out.
    pub fetch_release_date: &'a dyn Fn(&str) -> Result<(String, String), String>,
    /// Newest release of a git forge repository within a series (`[2, 7]`).
    pub fetch_repo_in_series: &'a dyn Fn(&str, &[u64]) -> Result<String, String>,
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
///
/// `service_git` is the forge repository the checkout's `_service` names (see
/// [`service_git_url`]), for an obs_scm source whose spec has no URL to read.
pub(crate) fn ecosystems(header: &spec::Header, service_git: Option<String>) -> Vec<Ecosystem> {
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
    // A bare tarball name is a guess at a PyPI project, which only a Python
    // package's name can be: a C library's `galera-4` must not reach PyPI.
    if header.python
        && let Some(pypi_name) = pypi::pypi_name_from_versioned_tarball(source, &header.version)
    {
        add(Ecosystem::Pypi { pypi_name });
    }
    let expanded = header.expanded_source.as_deref().unwrap_or_default();
    for line in [expanded, header.url.as_deref().unwrap_or_default()] {
        if let Some(url) = git_repo_url(line) {
            add(Ecosystem::GitRepo { url });
        }
    }
    if let Some(url) = service_git {
        add(Ecosystem::GitRepo { url });
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
    if let Some(gem) = header
        .name
        .strip_prefix("rubygem-")
        .filter(|g| !g.is_empty())
    {
        add(Ecosystem::Rubygems {
            gem: gem.to_string(),
        });
    }
    if let Some(dist) = crate::upstream_cpan::dist_of(header) {
        add(Ecosystem::Cpan { dist });
    }
    out
}

/// The forge repository the checkout's `obs_scm` service fetches the source
/// from, as a forge URL. An obs_scm source is made by OBS at build time, so
/// the spec alone names no repository for it. Only a live service counts: a
/// commented-out one is masked, a `disabled` one is skipped, and so is one
/// that does not fetch from git (`scm`). `tar_scm` is not read: a snapshot
/// packaged from one names its own version, not the newest tag.
pub(crate) fn service_git_url(service: &str) -> Option<String> {
    let masked = mask_xml_comments(service);
    service_elements(&masked, "obs_scm")
        .into_iter()
        .find_map(|at| {
            let params = source_params(service, &masked, at);
            let param = |name: &str| param_value(&params, name).filter(|v| !v.is_empty());
            let disabled =
                crate::obs_xml::attr(open_tag(&masked, at), "mode").as_deref() == Some("disabled");
            if disabled || param("scm").is_some_and(|scm| scm != "git") {
                return None;
            }
            git_repo_url(param("url")?)
        })
}

/// Where a spec's own `Source:` says its upstream is, without the guesses
/// [`ecosystems`] makes: a PyPI URL or wheel name, or a git forge. A package
/// that names neither is not looked up on PyPI by name, since a PyPI project
/// can share the name of an unrelated C library (PyPI's `aws-c-http` is a
/// placeholder at 0.0.1).
#[derive(Debug, PartialEq, Eq)]
pub enum DeclaredUpstream {
    Pypi,
    Git(String),
    Unknown,
}

pub fn declared_upstream(header: &spec::Header) -> DeclaredUpstream {
    let source = header.source.as_deref().unwrap_or_default();
    if pypi::pypi_name_from_source_url(source).is_some()
        || pypi::pypi_name_and_version_from_wheel_filename(source).is_some()
    {
        return DeclaredUpstream::Pypi;
    }
    let expanded = header.expanded_source.as_deref().unwrap_or_default();
    for line in [expanded, header.url.as_deref().unwrap_or_default()] {
        if let Some(url) = git_repo_url(line) {
            return DeclaredUpstream::Git(url);
        }
    }
    DeclaredUpstream::Unknown
}

/// Ask one ecosystem for its newest version. A lookup whose name or URL still
/// holds a macro is refused, and that text is named in the reason.
pub(super) fn lookup(ecosystem: &Ecosystem, lookups: &Lookups<'_>) -> Result<String, SkipReason> {
    let (tag, name) = match ecosystem {
        Ecosystem::Pypi { pypi_name } => ("PyPI", pypi_name),
        Ecosystem::GitRepo { url } => ("git", url),
        Ecosystem::Index { url } => ("release listing", url),
        Ecosystem::Repology { project } => ("Repology", project),
        Ecosystem::Rubygems { gem } => ("rubygems.org", gem),
        Ecosystem::Cpan { dist } => ("MetaCPAN", dist),
    };
    if spec::has_unexpanded_macro(name) {
        return Err(SkipReason::UnexpandedMacro {
            lookup: tag.into(),
            text: name.clone(),
        });
    }
    let latest = match ecosystem {
        Ecosystem::Pypi { pypi_name } => (lookups.fetch_pypi_latest)(pypi_name),
        Ecosystem::GitRepo { url } => (lookups.fetch_repo_latest)(url),
        Ecosystem::Index { url } => (lookups.fetch_index_latest)(url),
        Ecosystem::Repology { project } => (lookups.fetch_repology_latest)(project),
        Ecosystem::Rubygems { gem } => (lookups.fetch_gem_latest)(gem),
        Ecosystem::Cpan { dist } => (lookups.fetch_cpan_latest)(dist),
    };
    latest.map_err(|error| SkipReason::LookupFailed {
        ecosystem_tag: tag.into(),
        name: name.clone(),
        error,
    })
}

/// Whether `latest` is strictly newer than `packaged`.
pub fn is_newer(latest: &str, packaged: &str) -> bool {
    compare_versions(latest, packaged, Scheme::Generic) == pypi::VersionCompare::Newer
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
pub(crate) fn git_repo_url(source: &str) -> Option<String> {
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
pub(super) fn extract_url_like(source: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_dashed_release_says_what_to_do_next() {
        let text = SkipReason::NotAnRpmVersion {
            latest: "2025-10-15T17-29-55Z".into(),
        }
        .to_string();
        assert!(text.contains("2025-10-15T17-29-55Z"), "{text}");
        assert!(text.contains("not a valid RPM Version"), "{text}");
        assert!(text.contains("A maintainer must choose"), "{text}");
        assert!(text.contains("--to-version"), "{text}");
    }

    #[test]
    fn a_calendar_date_is_dated_in_either_form() {
        assert!(is_dated("2020-11-13"));
        assert!(is_dated("20260616"));
        assert!(is_dated("20240229"));
        assert!(!is_dated("3.1.2"));
        assert!(!is_dated("20.5"));
        assert!(!is_dated("202606161"));
        assert!(!is_dated("20261316"));
        assert!(!is_dated("21000101"));
    }

    #[test]
    fn a_date_inside_a_longer_version_is_not_a_date() {
        assert!(!is_dated("2.20191203"));
        assert!(!is_dated("2.0.20240101"));
        assert!(!is_dated("1.0+git20260101"));
        assert!(!is_dated("2020-11-13-rc1"));
        // A multi-byte character in place of a digit is not a date, and no panic.
        assert!(!is_dated("2020-1\u{e9}-13"));
    }

    #[test]
    fn a_day_the_month_does_not_have_is_not_a_date() {
        assert!(!is_dated("20240230"));
        assert!(!is_dated("20230229"));
        assert!(!is_dated("20230431"));
        assert!(is_dated("20000229"));
        assert!(!is_dated("19000229"));
        assert!(is_dated("2024-02-29"));
        assert!(!is_dated("2023-02-29"));
    }
}
