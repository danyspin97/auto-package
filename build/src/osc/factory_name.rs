//! Whether openSUSE:Factory already has a source package of some name: a
//! read-only existence check on its `_meta`, always asked of
//! [`FACTORY_API`] and never of whatever server osc is set up for.

use super::query::{ApiFailure, osc_api_checked_at};

/// The project whose package names a new package must not collide with.
pub const FACTORY: &str = "openSUSE:Factory";

/// The API server that holds [`FACTORY`]. A private OBS answers 404 for the
/// same path, which says nothing about Factory.
pub const FACTORY_API: &str = "https://api.opensuse.org";

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryName {
    /// OBS answered 404 `unknown_package`: no such package in Factory.
    Free,
    /// Factory has a package of that name.
    Taken,
    /// No answer (no osc, a timeout, a network or server failure, no account
    /// for [`FACTORY_API`], a 404 that does not say the package is missing, a
    /// name that is not a valid package name): neither free nor taken. The
    /// text says why.
    Unknown(String),
}

/// A lookup of one name in Factory, so a test can stand in for `osc`.
pub trait FactoryNames {
    fn check(&self, name: &str) -> FactoryName;
}

/// The check run through `osc -A <FACTORY_API> api`, with its one 60 s
/// timeout (the first answer is final).
pub struct OscFactory;

impl FactoryNames for OscFactory {
    fn check(&self, name: &str) -> FactoryName {
        check_with(name, |path| osc_api_checked_at(Some(FACTORY_API), path))
    }
}

/// Whether a 404's text says the package, not the project, is missing.
fn says_package_missing(text: &str) -> bool {
    let lower = text.to_lowercase();
    let package = lower.contains("unknown_package") || lower.contains("package not found:");
    let project = lower.contains("unknown_project") || lower.contains("project not found:");
    package && !project
}

/// The first line of `text`, trimmed to about 120 characters.
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(120) {
        Some((i, _)) => format!("{}...", &line[..i]),
        None => line.to_string(),
    }
}

/// [`FactoryName`] of `name`, asking `api` for the package's `_meta`. A name
/// that is not a plain OBS package name is never put into a path. Only a 404
/// whose text says `unknown_package` counts as free.
pub(super) fn check_with(
    name: &str,
    api: impl FnOnce(&str) -> Result<String, ApiFailure>,
) -> FactoryName {
    if !super::is_valid_obs_name(name) || name.contains(':') {
        return FactoryName::Unknown(format!("'{name}' is not a package name"));
    }
    let path = format!("/source/{FACTORY}/{}/_meta", super::query::enc(name));
    match api(&path) {
        Ok(_) => FactoryName::Taken,
        Err(ApiFailure::Http(404, m)) if says_package_missing(&m) => FactoryName::Free,
        Err(ApiFailure::Http(404, m)) => FactoryName::Unknown(format!(
            "404 that does not say the package is missing: {}",
            short(&m)
        )),
        Err(ApiFailure::Http(_, m) | ApiFailure::Other(m)) => FactoryName::Unknown(short(&m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(code: u16, text: &str) -> Result<String, ApiFailure> {
        Err(ApiFailure::Http(
            code,
            format!("exit 1: HTTP Error {code}: {text}"),
        ))
    }

    fn absent() -> Result<String, ApiFailure> {
        http(404, "Not Found\n<status code=\"unknown_package\"/>")
    }

    #[test]
    fn a_meta_document_means_taken() {
        assert_eq!(
            check_with("jay", |_| Ok("<package/>".into())),
            FactoryName::Taken
        );
    }

    #[test]
    fn an_unknown_package_404_means_free_and_asks_the_meta_path() {
        let mut asked = String::new();
        let got = check_with("jay", |p| {
            asked = p.to_string();
            absent()
        });
        assert_eq!(got, FactoryName::Free);
        assert_eq!(asked, "/source/openSUSE:Factory/jay/_meta");
    }

    #[test]
    fn a_404_for_the_project_or_without_a_code_is_not_free() {
        for text in [
            "Not Found\n<status code=\"unknown_project\"/>",
            "Project not found: openSUSE:Factory",
            "Not Found",
        ] {
            let got = check_with("jay", |_| http(404, text));
            assert!(matches!(got, FactoryName::Unknown(_)), "{text}");
        }
    }

    #[test]
    fn any_other_failure_is_unknown_in_one_short_line() {
        for code in [401, 403, 500, 503] {
            assert!(matches!(
                check_with("jay", |_| http(code, "x")),
                FactoryName::Unknown(_)
            ));
        }
        let long = format!("timed out\nsecond line {}", "x".repeat(300));
        let FactoryName::Unknown(t) = check_with("jay", |_| Err(ApiFailure::Other(long))) else {
            panic!()
        };
        assert_eq!(t, "timed out");
        let FactoryName::Unknown(t) =
            check_with("jay", |_| Err(ApiFailure::Other("y".repeat(300))))
        else {
            panic!()
        };
        assert!(t.len() <= 123);
    }

    #[test]
    fn a_bad_name_is_never_asked() {
        for bad in ["", "a/b", "../x", "a b", "x?y=1", "openSUSE:x", "-x"] {
            let got = check_with(bad, |_| panic!("asked for {bad:?}"));
            assert!(matches!(got, FactoryName::Unknown(_)), "{bad:?}");
        }
    }

    #[test]
    fn the_read_names_the_public_api_server_before_the_subcommand() {
        let args = super::super::query::api_args(Some(FACTORY_API), "/p");
        assert_eq!(args, ["-A", "https://api.opensuse.org", "api", "/p"]);
        assert_eq!(super::super::query::api_args(None, "/p"), ["api", "/p"]);
    }
}
