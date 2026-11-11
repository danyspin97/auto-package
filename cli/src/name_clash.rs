//! A bare name on the command line that means two different projects.
//!
//! `build jay` used to take the PyPI project called `jay` without looking
//! further, although the user meant the `jay` Wayland compositor. When PyPI
//! and a repository search both answer to the name, and PyPI does not point
//! at that repository, auto-package stops and lists both.

use crate::*;

/// Whether the clash check runs for a bare name. It does not when the name is
/// a repository (a URL, or `--url`), when an NVIDIA option is given, when
/// `--source-url` or the saved source map gives the name a source, or when the
/// name is NVIDIA's by its own spelling, or is one the build maps to a
/// repository itself (`ap_build::pipeline::is_known_mapped_name`).
/// `pypi:<name>` is not checked at all: the caller reads it with
/// `parse_request` and does not ask.
pub(crate) fn applies(name: &str, repo_given: bool, nvidia: bool, source_mapped: bool) -> bool {
    let lower = name.to_lowercase();
    !(repo_given || nvidia || source_mapped)
        && !ap_build::pipeline::is_nvidia_package_name(&lower)
        && !ap_build::pipeline::is_known_mapped_name(&lower)
}

/// A name as typed: the package it builds, the repository URL it names (if
/// any), and whether `pypi:` asked for the PyPI project.
#[derive(Debug, PartialEq)]
pub(crate) struct Request {
    pub(crate) name: String,
    pub(crate) repo: Option<String>,
    pub(crate) pypi: bool,
}

/// Read a name as typed, on the command line or in the TUI. `pypi:<name>`
/// builds the PyPI project; a repository URL names its repository; any other
/// text is a bare name. A retry is sent with the text its row was requested
/// with, so `pypi:` survives.
pub(crate) fn parse_request(raw: &str) -> Result<Request, String> {
    const PREFIX: &str = "pypi:";
    // The prefix is read without regard to case: `PYPI:Foo` is `Foo` on PyPI.
    let (text, pypi) = match raw.get(..PREFIX.len()) {
        Some(p) if p.eq_ignore_ascii_case(PREFIX) => (&raw[PREFIX.len()..], true),
        _ => (raw, false),
    };
    if looks_like_url(text) {
        if pypi {
            return Err(format!("`{raw}`: `pypi:` takes a package name, not a URL"));
        }
        return Ok(Request {
            name: repo_package_name(text),
            repo: Some(text.to_string()),
            pypi,
        });
    }
    // A colon is not in a PyPI name: `pypi:pypi:x` is a mistake, not a name.
    if pypi && text.contains(':') {
        return Err(format!(
            "`{raw}`: `pypi:` takes a bare package name, and `{text}` has a colon"
        ));
    }
    Ok(Request {
        name: text.to_string(),
        repo: None,
        pypi,
    })
}

/// Repositories named exactly `name` that PyPI's own source URL (if any) does
/// not point at. Empty means no clash: PyPI is the repository, or there is no
/// repository of that name. Only repositories on a known forge count: a
/// release directory on another host (a Savannah or ftp.gnu.org index) is not
/// a repository of the project.
pub(crate) fn clashing_repos(
    name: &str,
    pypi_source: Option<&str>,
    verified: &[String],
) -> Vec<String> {
    let want = ap_build::sources::normalize_name(name);
    // A repository's identity: its host without `www.`, and its project path.
    let key = |url: &str| {
        ap_ecosystem::forge::parse(url).map(|f| {
            let host = f.host.to_lowercase();
            let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
            (host, f.project_path.to_lowercase())
        })
    };
    let pypi_key = pypi_source.and_then(key);
    verified
        .iter()
        .filter(|url| {
            ap_ecosystem::forge::parse(url).is_some_and(|f| {
                ap_ecosystem::forge::is_known_host(&f.host)
                    && ap_build::sources::normalize_name(&f.repo_name) == want
            })
        })
        .filter(|url| pypi_key.is_none() || key(url) != pypi_key)
        .cloned()
        .collect()
}

/// The refusal text, naming both choices and how to take each.
pub(crate) fn clash_message(
    name: &str,
    version: &str,
    pypi_source: Option<&str>,
    repos: &[String],
) -> String {
    let pypi = match pypi_source {
        Some(s) => format!("PyPI project {name} {version} (its source: {s})"),
        None => format!("PyPI project {name} {version} (no source repository on a known forge)"),
    };
    let mut out = format!("`{name}` is ambiguous. It could be:\n  - the {pypi}\n");
    for r in repos {
        out.push_str(&format!("  - the repository {r}\n"));
    }
    out.push_str(&format!(
        "Re-run as `build pypi:{name}` to build the PyPI project, or give the repository \
         as the name (`build {}`), or in the TUI type `pypi:{name}` instead.",
        repos.first().map(String::as_str).unwrap_or("<url>")
    ));
    out
}

/// The repositories a clash can be with: the vouched ones, and the name search
/// hits that exist. A generated guess (github.com/<name>/<name> and the like)
/// is not one, even when it exists.
pub(crate) fn clash_candidates(found: &ap_build::pipeline::RepoCandidates) -> Vec<String> {
    found
        .verified
        .iter()
        .chain(&found.unvouched_search_hits)
        .cloned()
        .collect()
}

/// The warning for a name whose clash check could not read PyPI's answer.
pub(crate) fn fetch_failed_warning(name: &str, error: &str) -> String {
    format!("{error}; {name} is not checked for a clash")
}

/// The warning for a name whose PyPI source is on a host that is not a forge.
pub(crate) fn non_forge_skip_warning(name: &str) -> String {
    format!(
        "PyPI names a source (a Source or Repository link) on a host that is not \
         a known forge, so it cannot be compared with the repositories found for \
         {name}; {name} is not checked for a clash"
    )
}

/// The reporter of a clash check run before the build has one (`build <name>`):
/// its warnings go to stderr, so a skipped check is not silent.
pub(crate) struct WarningsToStderr;

impl Reporter for WarningsToStderr {
    fn report(&self, event: &Event) {
        if let Event::Warning { message, .. } = event {
            ap_core::diag!("warning: {message}");
        }
    }
}

/// Whether the clash check is skipped: PyPI gives no source on a known forge,
/// but names one under a source key on a host that is not a forge. Its upstream
/// cannot be compared with a repository, so the name is not refused.
pub(crate) fn skips_for_non_forge_source(pypi_source: Option<&str>, json: &str) -> bool {
    pypi_source.is_none() && ap_ecosystem::pypi::names_non_forge_url(json)
}

/// The PyPI source as the clash comparison uses it. A source that redirects (a
/// renamed or transferred repository, a changed organisation) is compared at
/// the canonical forge URL its redirects end at, `resolved`. Without one that
/// is a known forge, PyPI's own URL is used, as before.
pub(crate) fn comparable_source(
    pypi_source: Option<&str>,
    resolved: Option<&str>,
) -> Option<String> {
    resolved
        .and_then(ap_ecosystem::forge::parse)
        .filter(|f| ap_ecosystem::forge::is_known_host(&f.host))
        .map(|f| f.canonical_https())
        .or_else(|| pypi_source.map(str::to_string))
}

/// Refuse `name` when PyPI and a repository search both claim it.
pub(crate) fn check(name: &str, reporter: &dyn Reporter) -> color_eyre::Result<()> {
    let Some(version) = pypi_latest_version(name, reporter) else {
        return Ok(());
    };
    // No agent here: a model must not be asked whether a name is ambiguous.
    let found = ap_build::pipeline::find_source_repo_candidates_without_agent(name, name);
    let repos_found = clash_candidates(&found);
    if repos_found.is_empty() {
        return Ok(());
    }
    // Only a PyPI answer that was read can show a clash: a failed or missing
    // fetch leaves nothing to compare against, so the name is not refused.
    let json = match ap_build::pipeline::fetch_pypi_json(name, &version) {
        Ok(Some(json)) => json,
        Ok(None) => return Ok(()),
        Err(e) => {
            reporter.report(&Event::Warning {
                step: None,
                message: fetch_failed_warning(name, &e.to_string()),
            });
            return Ok(());
        }
    };
    let pypi_source = ap_ecosystem::pypi::source_url_from_json(&json);
    // PyPI names an upstream, but not one on a known forge: nothing here can
    // say whether it is one of the repositories found, so the name is not
    // refused on that account.
    if skips_for_non_forge_source(pypi_source.as_deref(), &json) {
        reporter.report(&Event::Warning {
            step: None,
            message: non_forge_skip_warning(name),
        });
        return Ok(());
    }
    let repos = clashing_repos(name, pypi_source.as_deref(), &repos_found);
    if repos.is_empty() {
        return Ok(());
    }
    // A clash stands only when PyPI's URL, followed through redirects, is not
    // the repository either. Any failure to resolve it leaves the clash.
    let resolved = pypi_source.as_deref().and_then(ap_build::http::final_url);
    let source = comparable_source(pypi_source.as_deref(), resolved.as_deref());
    let repos = clashing_repos(name, source.as_deref(), &repos_found);
    if repos.is_empty() {
        return Ok(());
    }
    Err(color_eyre::eyre::eyre!(clash_message(
        name,
        &version,
        pypi_source.as_deref(),
        &repos
    )))
}

/// A TUI request as the run admits it: read, its name validated, and checked
/// for a clash under the same gate as `build`. `pypi:<name>` is not checked.
pub(crate) fn admit(
    raw: &str,
    source_urls: &ap_build::sources::SourceMap,
    nvidia: bool,
    reporter: &dyn Reporter,
) -> color_eyre::Result<Request> {
    let req = parse_request(raw).map_err(|e| color_eyre::eyre::eyre!(e))?;
    crate::validate_package_name(&req.name).map_err(|e| color_eyre::eyre::eyre!(e))?;
    let mapped = source_urls.get(&req.name).is_some();
    if !req.pypi && applies(&req.name, req.repo.is_some(), nvidia, mapped) {
        check(&req.name, reporter)?;
    }
    Ok(req)
}

/// The package a request names, for matching it to a failure: `pypi:<name>`,
/// a repository URL and a bare name of the same package give the same name.
pub(crate) fn requested_name(text: &str) -> String {
    parse_request(text)
        .map(|r| r.name)
        .unwrap_or_else(|_| text.to_string())
}

/// The origin of a root build given as `pypi:<name>`: a retry of its row sends
/// the same text, so the PyPI choice survives. None for any other root.
pub(crate) fn root_text(name: &str, pypi: bool) -> Option<String> {
    pypi.then(|| format!("pypi:{name}"))
}
