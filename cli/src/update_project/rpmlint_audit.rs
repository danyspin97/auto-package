//! The rpmlintrc files of a package that `lint` judges, and the project they
//! are read from: the branch when `lint` continues from it (see
//! `existing_lint`), else the source. A file a `Source:` line of the spec
//! names is judged for convention 39. A file no `Source:` line names is left
//! out by the publish plan, so it is reported as well, and judged the same way.

use super::existing::scheme_of_ecosystem;
use super::existing_lint::{Continuation, decide_versions, origin_version};
use super::*;
use ap_build::quality::{Finding, Kind};
use ap_build::rpmlintrc_lint;
use ap_build::version::Scheme;

/// Whether `name` is an rpmlint filter file: `<name>-rpmlintrc` or `<name>.rpmlintrc`.
fn is_rpmlintrc(name: &str) -> bool {
    name.ends_with("-rpmlintrc") || name.ends_with(".rpmlintrc")
}

/// The rpmlintrc files of `listed` (the files of a package) split into those the
/// spec names and those it does not, each in the order `listed` has them.
pub(super) fn split_files(spec: &str, listed: &[String]) -> (Vec<String>, Vec<String>) {
    let named = rpmlintrc_lint::spec_rpmlintrc_files(spec);
    let unnamed = listed
        .iter()
        .filter(|f| is_rpmlintrc(f) && !named.contains(f))
        .cloned()
        .collect();
    (named, unnamed)
}

/// Whether `lint` of `pkg` continues from `branch` rather than from `origin`.
fn continues_from_branch(
    origin: &str,
    branch: &str,
    pkg: &str,
    ecosystem: Option<&Ecosystem>,
    emit: Emit<'_>,
) -> bool {
    let scheme = ecosystem.map_or(Scheme::Generic, scheme_of_ecosystem);
    matches!(
        decide_versions(scheme, &|| origin_version(origin, pkg, emit), &|| {
            ap_build::update_project::branch_version(branch, pkg, emit)
        },),
        Continuation::Branch { .. }
    )
}

/// The branch whose rpmlintrc files are judged: `branch` when `lint` may
/// continue from it and `continued` says the package does. `continued` is
/// only asked when `continues` holds.
fn judged_branch<'a>(
    branch: Option<&'a str>,
    continues: bool,
    continued: impl FnOnce(&str) -> bool,
) -> Option<&'a str> {
    branch.filter(|b| continues && continued(b))
}

/// The rpmlintrc findings of `pkg`. `source_spec` is the spec the audit read
/// from the source; `continues` says whether `lint` may continue from `branch`.
/// Files that cannot be read are reported through `warn`.
#[allow(clippy::too_many_arguments)]
pub(super) fn findings(
    source: &str,
    branch: Option<&str>,
    continues: bool,
    pkg: &str,
    ecosystem: Option<&Ecosystem>,
    source_spec: &str,
    emit: Emit<'_>,
    warn: &dyn Fn(String),
) -> Vec<Finding> {
    let branch = judged_branch(branch, continues, |b| {
        continues_from_branch(source, b, pkg, ecosystem, emit)
    });
    let (project, spec) = match branch {
        Some(b) => match update::spec_for_package(b, pkg, emit) {
            Ok(text) => (b, text),
            Err(e) => {
                warn(format!(
                    "could not read the spec of {pkg} in {b}, so its rpmlintrc is read from {source}: {e}"
                ));
                (source, source_spec.to_string())
            }
        },
        None => (source, source_spec.to_string()),
    };
    let listed = osc::list_package_files(project, pkg, emit).unwrap_or_else(|| {
        warn(format!(
            "could not list the files of {pkg} in {project}, so its unnamed rpmlintrc is not checked"
        ));
        Vec::new()
    });
    let (named, unnamed) = split_files(&spec, &listed);
    let mut out = Vec::new();
    for file in &named {
        let Some(text) = osc::cat_file(project, pkg, file, emit) else {
            warn(format!(
                "could not read {file} of {pkg}, so its rpmlintrc is not checked"
            ));
            continue;
        };
        out.extend(rpmlintrc_lint::audit(file, &text));
    }
    // An unnamed file is only reported: it is not judged, and nothing edits it.
    for file in &unnamed {
        let Some(text) = osc::cat_file(project, pkg, file, emit) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push(Finding {
            kind: Kind::RpmlintrcUnnamed,
            detail: format!(
                "{file}: no Source: line of the spec names it, so the publish plan leaves it out"
            ),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{judged_branch, split_files};

    #[test]
    fn the_branch_is_judged_only_when_lint_continues_from_it() {
        let branch = Some("home:u:branch");
        let yes = |_: &str| true;
        assert_eq!(judged_branch(branch, true, yes), branch);
        assert_eq!(judged_branch(branch, false, |_| unreachable!()), None);
        assert_eq!(judged_branch(branch, true, |_| false), None);
        assert_eq!(judged_branch(None, true, |_| unreachable!()), None);
    }

    const SPEC: &str =
        "Name: fwupd-efi\nVersion: 1.9\nSource0: x.tar.gz\nSource1: fwupd-efi-rpmlintrc\n";

    #[test]
    fn named_and_unnamed_rpmlintrcs_are_split() {
        let listed: Vec<String> = [
            "106.patch",
            "fwupd-efi-rpmlintrc",
            "fwupd-efi.rpmlintrc",
            "series",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (named, unnamed) = split_files(SPEC, &listed);
        assert_eq!(named, ["fwupd-efi-rpmlintrc"]);
        assert_eq!(unnamed, ["fwupd-efi.rpmlintrc"]);
    }

    #[test]
    fn a_package_without_rpmlintrc_has_no_files() {
        let listed = vec!["a.spec".to_string(), "a.tar.gz".to_string()];
        let (named, unnamed) = split_files("Name: a\nVersion: 1\n", &listed);
        assert!(named.is_empty() && unnamed.is_empty());
    }
}
