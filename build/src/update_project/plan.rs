use super::*;

/// One package to branch and update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The package's name in `origin`, in the spelling OBS lists.
    pub pkg: String,
    /// The project it is branched from, and where its submit request goes.
    pub origin: String,
    pub ecosystem: Ecosystem,
    pub old_version: String,
    pub new_version: String,
    /// The package this one was pulled in for (`--update-deps`).
    pub dependency_of: Option<String>,
    /// What `lint` found wrong with the package, one line each.
    /// Empty for an update, where `old_version` and `new_version` differ.
    pub findings: Vec<String>,
}

/// Whether `pkg` is named by `list`, compared the way the rest of the tool
/// compares package names, so `Foo_Bar` finds `foo-bar`.
pub fn name_listed(list: &[String], pkg: &str) -> bool {
    let key = osc::fuzzy_key(pkg);
    list.iter().any(|n| osc::fuzzy_key(n) == key)
}

/// The package to branch for a name `osc ls` listed: a multibuild flavor
/// (`apache-arrow:python-pyarrow`) is part of its source package.
pub fn branch_name(listed: &str) -> &str {
    listed.split(':').next().unwrap_or(listed)
}

/// The packages of a plan that have a newer upstream release, minus
/// `exclude`, each once.
pub fn items_from_plan(origin: &str, candidates: &[Candidate], exclude: &[String]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for c in candidates {
        let Decision::Update {
            ecosystem,
            old_version,
            new_version,
        } = &c.decision
        else {
            continue;
        };
        let pkg = branch_name(&c.rpm_name);
        // A version found in a release listing or on Repology is reported, but
        // the pipeline cannot build from what they name.
        if !ecosystem.can_build() || name_listed(exclude, pkg) || items.iter().any(|i| i.pkg == pkg)
        {
            continue;
        }
        items.push(Item {
            pkg: pkg.to_string(),
            origin: origin.to_string(),
            ecosystem: ecosystem.clone(),
            old_version: old_version.clone(),
            new_version: new_version.clone(),
            dependency_of: None,
            findings: Vec::new(),
        });
    }
    items
}

/// Whether the build pipeline would build `item` under the name it already
/// has. A PyPI package is named by the pipeline from its PyPI name
/// (`python-<name>`); one that is called something else would be built as a
/// second package next to the branched one. A repository package is given its
/// name explicitly, so it never mismatches.
pub fn check_pipeline_name(item: &Item) -> Result<(), String> {
    match &item.ecosystem {
        Ecosystem::Pypi { pypi_name } => {
            let built = osc::rpm_name(pypi_name);
            if osc::fuzzy_key(&built) == osc::fuzzy_key(&item.pkg) {
                Ok(())
            } else {
                Err(format!(
                    "the build pipeline would name it {built}, not {}, and create a second package",
                    item.pkg
                ))
            }
        }
        Ecosystem::GitRepo { .. } => Ok(()),
        // Only reported, never built, so there is no name to mismatch.
        Ecosystem::Index { .. }
        | Ecosystem::Repology { .. }
        | Ecosystem::Rubygems { .. }
        | Ecosystem::Cpan { .. } => Ok(()),
    }
}

/// What a dry run knows.
pub struct DryRun<'a> {
    pub source: &'a str,
    pub branch_project: &'a str,
    pub branch_exists: bool,
    pub items: &'a [Item],
    /// Build paths that would be added to the branch project.
    pub paths: &'a [PathAddition],
    pub submit: bool,
}

/// What a dry run prints after the update plan: the branch project, the build
/// paths that would be added, and the requests that would be created.
pub fn dry_run_lines(d: &DryRun<'_>) -> Vec<String> {
    let mut out = vec![format!(
        "dry run: nothing is branched, built, edited or submitted; {} stays untouched",
        d.source
    )];
    out.push(format!(
        "branch project: {} ({})",
        d.branch_project,
        if d.branch_exists {
            "exists"
        } else {
            "would be created by the first branch"
        }
    ));
    out.push(format!(
        "packages to branch and update ({}):",
        d.items.len()
    ));
    for i in d.items {
        out.push(format!(
            "  {}  {} -> {}  (from {}{}{})",
            i.pkg,
            i.old_version,
            i.new_version,
            i.origin,
            i.dependency_of
                .as_ref()
                .map(|p| format!(", needed by {p}"))
                .unwrap_or_default(),
            match crate::branch_revision::direction_note(&i.old_version, &i.new_version) {
                "" => String::new(),
                note => format!(", {note}"),
            }
        ));
    }
    if d.paths.is_empty() {
        out.push("extra build paths: none needed".to_string());
    } else {
        out.push(format!("extra build paths ({}):", d.paths.len()));
        for p in d.paths {
            out.push(format!(
                "  {}: + {}/{}",
                p.repository, p.project, p.path_repository
            ));
        }
    }
    if d.submit {
        out.push(format!(
            "submit requests that would be created, once each build succeeds ({}):",
            d.items.len()
        ));
        for i in d.items {
            out.push(format!(
                "  {}/{} -> {}/{}",
                d.branch_project, i.pkg, i.origin, i.pkg
            ));
        }
    } else {
        out.push("submit requests: none (--submit not given)".to_string());
    }
    out
}
