//! How each package of a run is routed once the audit or the sweep hands it on:
//! committed without a build, or built, and the branch leftovers the tool cleans
//! in it. The dry run says the same routing the run takes.

use std::collections::HashMap;

/// What the pre-pass decided for one package.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Route {
    /// Its findings are committed without a build (see `audit::is_style_only`).
    pub(super) style_only: bool,
    /// The branch leftovers the tool cleans, by finding name
    /// (`changes.dependency-bullet`, `FLAG`).
    pub(super) tool: Vec<String>,
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

    /// The branch leftovers the tool cleans in `pkg`.
    pub(super) fn tool(&self, pkg: &str) -> &[String] {
        self.0.get(pkg).map_or(&[], |r| r.tool.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_is_mechanical_only_when_its_findings_are_style_only() {
        let mut routes = Routes::default();
        routes.insert(
            "cookiecutter",
            Route {
                style_only: true,
                tool: vec!["changes.dependency-bullet".to_string(), "FLAG".to_string()],
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
