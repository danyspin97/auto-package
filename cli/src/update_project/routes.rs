//! How each package of a run is routed once the audit or the sweep hands it on:
//! committed without a build, or built, and the branch leftovers the tool cleans
//! in it. The dry run says the same routing the run takes.

use std::collections::HashMap;

use ap_build::quality::Finding;

/// Whether a package sent to the pipeline is tidied first: it has a mechanical
/// finding of its own, which the tidy fixes before the build. Not when an
/// inherited mechanical finding would be rewritten too (`tidy_blocked`, which
/// the default scope leaves alone unless `--fix` is given).
pub(super) fn tidy_first_of(findings: &[Finding], tidy_blocked: bool) -> bool {
    !tidy_blocked && findings.iter().any(|f| f.kind.is_mechanical())
}

/// What the pre-pass decided for one package.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Route {
    /// Its findings are committed without a build (see `audit::is_style_only`).
    pub(super) style_only: bool,
    /// Its mechanical findings are tidied before the pipeline builds it (see
    /// `audit::tidy_first`).
    pub(super) tidy_first: bool,
    /// The branch leftovers the tool cleans, by finding name
    /// (`changes.dependency-bullet`, `FLAG`).
    pub(super) tool: Vec<String>,
    /// The versions the tool fills the missing upstream recap for (`1.7.5..1.9.0`).
    pub(super) recap: Option<String>,
    /// The patches whose headers the tidy rewrites (see `patch_headers::split`).
    pub(super) headers: Vec<String>,
}

/// The routes of the packages prepared in a run, by package name.
#[derive(Debug, Default)]
pub(super) struct Routes(HashMap<String, Route>);

impl Routes {
    pub(super) fn insert(&mut self, pkg: &str, route: Route) {
        self.0.insert(pkg.to_string(), route);
    }

    /// Whether `pkg` is committed without a build.
    pub(super) fn mechanical(&self, pkg: &str) -> bool {
        self.0.get(pkg).is_some_and(|r| r.style_only)
    }

    /// Whether the pipeline builds `pkg` after a tidy of its mechanical findings.
    pub(super) fn tidy_first(&self, pkg: &str) -> bool {
        self.0.get(pkg).is_some_and(|r| r.tidy_first)
    }

    /// The patches of `pkg` whose headers the tidy rewrites.
    pub(super) fn headers(&self, pkg: &str) -> &[String] {
        self.0.get(pkg).map_or(&[], |r| r.headers.as_slice())
    }

    /// The branch leftovers the tool cleans in `pkg`.
    pub(super) fn tool(&self, pkg: &str) -> &[String] {
        self.0.get(pkg).map_or(&[], |r| r.tool.as_slice())
    }

    /// The versions the tool fills the upstream recap of `pkg` for, if it does.
    pub(super) fn recap(&self, pkg: &str) -> Option<&str> {
        self.0.get(pkg).and_then(|r| r.recap.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tidy_runs_first_for_an_own_mechanical_finding_not_an_inherited_one() {
        let finding = |kind| Finding {
            kind,
            detail: "x".to_string(),
        };
        let group = [finding(ap_build::quality::Kind::SpecHygiene)];
        let rpm = finding(ap_build::quality::Kind::RpmlintError);
        // The own mechanical finding is tidied beside an own agent-free one.
        assert!(tidy_first_of(&[rpm.clone(), group[0].clone()], false));
        // Nothing mechanical of its own: nothing to tidy.
        assert!(!tidy_first_of(std::slice::from_ref(&rpm), false));
        // An inherited mechanical finding blocks the tidy (the default scope).
        assert!(!tidy_first_of(&[rpm, group[0].clone()], true));
    }

    #[test]
    fn a_package_is_mechanical_only_when_its_findings_are_style_only() {
        let mut routes = Routes::default();
        routes.insert(
            "cookiecutter",
            Route {
                style_only: true,
                headers: Vec::new(),
                tidy_first: false,
                tool: vec!["changes.dependency-bullet".to_string(), "FLAG".to_string()],
                recap: None,
            },
        );
        routes.insert("pyenv", Route::default());
        assert!(routes.mechanical("cookiecutter"));
        assert_eq!(
            routes.tool("cookiecutter"),
            ["changes.dependency-bullet", "FLAG"]
        );
        assert!(!routes.mechanical("pyenv"));
        assert!(routes.tool("pyenv").is_empty());
        assert!(!routes.mechanical("unknown"));
    }
}
