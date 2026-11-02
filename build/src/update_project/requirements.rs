//! Capability requirements that no repository of the branch's build paths
//! provides, as an update reports them before it commits the package.

use std::collections::{BTreeMap, BTreeSet};

/// A capability a package needs and that no path provides in one repository
/// the branch builds in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingRequirement {
    pub pkg: String,
    /// The requirement as the spec writes it: `pkgconfig(miral) >= 5.5`.
    pub requirement: String,
    pub repository: String,
    pub arch: String,
}

/// The summary lines: one per requirement and repository, naming the packages
/// that need it. Each pair is listed once however many packages repeat it.
pub fn requirement_lines(missing: &[MissingRequirement]) -> Vec<String> {
    let mut grouped: BTreeMap<(&str, &str, &str), BTreeSet<&str>> = BTreeMap::new();
    for m in missing {
        grouped
            .entry((&m.requirement, &m.repository, &m.arch))
            .or_default()
            .insert(&m.pkg);
    }
    grouped
        .into_iter()
        .map(|((req, repo, arch), pkgs)| {
            let pkgs: Vec<&str> = pkgs.into_iter().collect();
            format!("{req} in {repo} ({arch}), needed by {}", pkgs.join(", "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(pkg: &str, req: &str, repo: &str) -> MissingRequirement {
        MissingRequirement {
            pkg: pkg.into(),
            requirement: req.into(),
            repository: repo.into(),
            arch: "x86_64".into(),
        }
    }

    #[test]
    fn a_requirement_repeated_by_packages_is_listed_once() {
        let lines = requirement_lines(&[
            missing("fuzzel", "pkgconfig(pixman-1) >= 0.46", "16.0"),
            missing("wlroots", "pkgconfig(pixman-1) >= 0.46", "16.0"),
            missing("fuzzel", "pkgconfig(pixman-1) >= 0.46", "16.0"),
            missing("miriway", "pkgconfig(miral) >= 5.5", "16.1"),
        ]);
        assert_eq!(
            lines,
            [
                "pkgconfig(miral) >= 5.5 in 16.1 (x86_64), needed by miriway",
                "pkgconfig(pixman-1) >= 0.46 in 16.0 (x86_64), needed by fuzzel, wlroots",
            ]
        );
    }

    #[test]
    fn the_same_requirement_in_two_repositories_is_two_lines() {
        let lines = requirement_lines(&[
            missing("a", "pkgconfig(x)", "16.0"),
            missing("a", "pkgconfig(x)", "16.1"),
        ]);
        assert_eq!(lines.len(), 2);
    }
}
