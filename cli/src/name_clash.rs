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
/// name is NVIDIA's by its own spelling. `pypi:<name>` is not checked at all:
/// the caller reads it with `parse_request` and does not ask.
pub(crate) fn applies(name: &str, repo_given: bool, nvidia: bool, source_mapped: bool) -> bool {
    !(repo_given || nvidia || source_mapped)
        && !ap_build::pipeline::is_nvidia_package_name(&name.to_lowercase())
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
                message: format!("{e}; {name} is not checked for a clash"),
            });
            return Ok(());
        }
    };
    let pypi_source = ap_ecosystem::pypi::source_url_from_json(&json);
    let repos = clashing_repos(name, pypi_source.as_deref(), &repos_found);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(urls: &[&str]) -> Vec<String> {
        urls.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_repo_pypi_does_not_point_at_clashes() {
        let got = clashing_repos(
            "jay",
            Some("https://github.com/someone/jay-py"),
            &v(&["https://github.com/mahkoh/jay"]),
        );
        assert_eq!(got, v(&["https://github.com/mahkoh/jay"]));
    }

    #[test]
    fn pypi_pointing_at_the_repo_is_no_clash() {
        let got = clashing_repos(
            "requests",
            Some("https://github.com/Psf/Requests"),
            &v(&["https://github.com/psf/requests"]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn pypi_with_no_source_clashes_with_an_exact_repo() {
        let got = clashing_repos("jay", None, &v(&["https://github.com/mahkoh/jay"]));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn a_differently_named_repo_is_not_a_clash() {
        let got = clashing_repos("jay", None, &v(&["https://github.com/x/jay-bindings"]));
        assert!(got.is_empty());
    }

    #[test]
    fn a_repository_named_by_url_or_flag_is_not_checked() {
        // `build <url>` and `--url` both give a repository: nothing to ask.
        assert!(!applies("jay", true, false, false));
    }

    #[test]
    fn an_nvidia_option_is_not_checked() {
        assert!(!applies("jay", false, true, false));
    }

    #[test]
    fn a_source_for_the_name_is_not_checked() {
        assert!(!applies("jay", false, false, true));
    }

    #[test]
    fn an_nvidia_named_package_is_not_checked() {
        for name in [
            "nvidia-cudnn",
            "cuda-cublas",
            "cuda_nvrtc",
            "NVIDIA",
            "nvidia",
        ] {
            assert!(!applies(name, false, false, false), "{name}");
        }
    }

    #[test]
    fn a_bare_name_is_checked() {
        assert!(applies("jay", false, false, false));
        // Not NVIDIA's: `nvim` and `nvector` only start with `nv`.
        assert!(applies("nvim", false, false, false));
    }

    #[test]
    fn a_repo_with_a_www_host_is_the_same_as_its_bare_host() {
        let got = clashing_repos(
            "jay",
            Some("https://github.com/someone/jay-py"),
            &v(&["https://www.github.com/mahkoh/jay"]),
        );
        assert_eq!(got, v(&["https://www.github.com/mahkoh/jay"]));
        let same = clashing_repos(
            "jay",
            Some("https://www.github.com/mahkoh/jay"),
            &v(&["https://github.com/mahkoh/jay"]),
        );
        assert!(same.is_empty());
    }

    #[test]
    fn a_git_suffix_and_the_ssh_spelling_name_the_same_repo() {
        let got = clashing_repos(
            "jay",
            Some("git@github.com:mahkoh/jay.git"),
            &v(&["https://github.com/mahkoh/jay.git"]),
        );
        assert!(got.is_empty());
        let got = clashing_repos("jay", None, &v(&["git@github.com:mahkoh/jay.git"]));
        assert_eq!(got, v(&["git@github.com:mahkoh/jay.git"]));
    }

    #[test]
    fn an_index_directory_on_another_host_is_not_a_repository() {
        let got = clashing_repos(
            "jay",
            None,
            &v(&[
                "https://ftp.gnu.org/gnu/jay",
                "https://savannah.gnu.org/projects/jay",
            ]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn underscores_and_dots_in_the_name_match_dashes_in_the_repo() {
        let got = clashing_repos("jay_bird", None, &v(&["https://github.com/x/jay-bird"]));
        assert_eq!(got.len(), 1);
        let got = clashing_repos(
            "zope.interface",
            None,
            &v(&["https://github.com/x/zope-interface"]),
        );
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn every_exact_name_repo_pypi_does_not_point_at_is_listed() {
        let got = clashing_repos(
            "jay",
            Some("https://github.com/someone/jay-py"),
            &v(&[
                "https://github.com/mahkoh/jay",
                "https://gitlab.com/other/jay",
                "https://github.com/x/jay-bindings",
            ]),
        );
        assert_eq!(
            got,
            v(&[
                "https://github.com/mahkoh/jay",
                "https://gitlab.com/other/jay"
            ])
        );
    }

    #[test]
    fn a_generated_guess_is_not_a_clash_but_an_exact_search_hit_is() {
        use ap_build::pipeline::RepoCandidates;
        let guess = RepoCandidates {
            verified: vec![],
            guessed: v(&[
                "https://github.com/python/jay",
                "https://github.com/jay/jay",
            ]),
            unvouched_search_hits: vec![],
        };
        assert!(clashing_repos("jay", None, &clash_candidates(&guess)).is_empty());
        let hit = RepoCandidates {
            verified: vec![],
            guessed: vec![],
            unvouched_search_hits: v(&["https://github.com/x/jay"]),
        };
        assert_eq!(
            clashing_repos("jay", None, &clash_candidates(&hit)),
            v(&["https://github.com/x/jay"])
        );
    }

    #[test]
    fn the_pypi_line_is_true_with_and_without_a_source() {
        let with = clash_message(
            "jay",
            "1.0",
            Some("https://github.com/someone/jay-py"),
            &v(&["https://github.com/x/jay"]),
        );
        assert!(with.contains("(its source: https://github.com/someone/jay-py)"));
        let without = clash_message("jay", "1.0", None, &v(&["https://github.com/x/jay"]));
        assert!(without.contains("(no source repository on a known forge)"));
        assert!(!without.contains("names no source"));
    }

    fn request(raw: &str) -> Request {
        parse_request(raw).unwrap()
    }

    #[test]
    fn a_pypi_prefix_is_the_pypi_project_under_the_bare_name() {
        assert_eq!(
            request("pypi:jay"),
            Request {
                name: "jay".into(),
                repo: None,
                pypi: true
            }
        );
        assert_eq!(
            request("jay"),
            Request {
                name: "jay".into(),
                repo: None,
                pypi: false
            }
        );
    }

    #[test]
    fn the_pypi_prefix_is_read_without_regard_to_case() {
        for raw in ["PYPI:Foo", "Pypi:Foo", "pYpI:Foo"] {
            let r = request(raw);
            assert_eq!((r.name.as_str(), r.pypi), ("Foo", true), "{raw}");
        }
        // Not a prefix at all: a bare name that does not start with `pypi:`.
        assert_eq!(request("pypí:x").pypi, false);
    }

    #[test]
    fn a_second_colon_after_the_prefix_is_refused() {
        assert!(parse_request("pypi:pypi:x").is_err());
        assert!(parse_request("PYPI:pypi:x").is_err());
    }

    #[test]
    fn the_stripped_pypi_name_is_a_valid_package_name() {
        assert!(crate::validate_package_name(&request("pypi:jay").name).is_ok());
    }

    #[test]
    fn a_request_names_its_package_under_any_spelling() {
        assert_eq!(requested_name("PYPI:jay"), "jay");
        assert_eq!(requested_name("jay"), "jay");
        assert_eq!(requested_name("https://github.com/mahkoh/jay.git"), "jay");
    }

    #[test]
    fn the_root_text_of_a_pypi_build_is_read_back_as_pypi() {
        assert_eq!(root_text("jay", true).as_deref(), Some("pypi:jay"));
        assert_eq!(root_text("jay", false), None);
        let r = request(&root_text("jay", true).unwrap());
        assert!(r.pypi && r.name == "jay" && r.repo.is_none());
    }

    #[test]
    fn a_repository_url_names_its_repository() {
        let r = request("https://github.com/mahkoh/jay");
        assert_eq!(r.name, "jay");
        assert_eq!(r.repo.as_deref(), Some("https://github.com/mahkoh/jay"));
        assert!(!r.pypi);
    }

    #[test]
    fn pypi_with_a_url_or_no_name_is_refused() {
        assert!(parse_request("pypi:https://github.com/mahkoh/jay").is_err());
        assert!(parse_request("pypi:git@github.com:mahkoh/jay.git").is_err());
        // An empty name is left to the package-name check.
        assert_eq!(request("pypi:").name, "");
    }

    #[test]
    fn the_message_offers_both_choices() {
        let m = clash_message("jay", "1.0", None, &v(&["https://github.com/mahkoh/jay"]));
        assert!(m.contains("build pypi:jay") && m.contains("build https://github.com/mahkoh/jay"));
        assert!(m.contains("type `pypi:jay`"));
    }
}
