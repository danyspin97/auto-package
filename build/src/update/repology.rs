//! Repology as a second opinion on a package's newest version. A package whose
//! primary source is not Repology is also compared with Repology's newest
//! version. Repology newer in the major or minor number means the two sources
//! disagree about which release to package, so a maintainer picks it; a
//! difference only in the patch level is a note in the plan.

use super::*;

/// What Repology's newest version says about the version the primary source
/// gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Nothing to say: Repology agrees, is older, or has no answer.
    Agree,
    /// Repology is newer only in the patch level.
    PatchLevel { repology: String },
    /// Repology is newer in the major or minor number.
    Disagrees { repology: String },
}

/// The verdict for `target`, the newest version of the primary source, against
/// the newest version `fetch` returns for `project` on Repology. Pure: the
/// lookup is passed in, so a test can answer it without the network.
pub(crate) fn verdict(
    project: &str,
    target: &str,
    scheme: Scheme,
    fetch: &dyn Fn(&str) -> Result<String, String>,
) -> Verdict {
    let Ok(repology) = fetch(project) else {
        return Verdict::Agree;
    };
    if plan::compare_versions(&repology, target, scheme) != pypi::VersionCompare::Newer {
        return Verdict::Agree;
    }
    let (theirs, ours) = (
        version::leading_numbers(&repology),
        version::leading_numbers(target),
    );
    let part = |numbers: &[u64], i: usize| numbers.get(i).copied().unwrap_or(0);
    if part(&theirs, 0) != part(&ours, 0) || part(&theirs, 1) != part(&ours, 1) {
        Verdict::Disagrees { repology }
    } else {
        Verdict::PatchLevel { repology }
    }
}

/// Apply the Repology check to a package's decision. Only an update from a
/// source other than Repology is checked: its target is what would be built.
/// A disagreement in the major or minor number turns the update into a skip,
/// and a patch-level one comes back as a note.
pub(super) fn settle(
    decision: Decision,
    project: &str,
    lookups: &Lookups<'_>,
) -> (Decision, Vec<String>) {
    let (source, target, scheme) = match &decision {
        Decision::Update {
            ecosystem,
            new_version,
            ..
        } if !matches!(ecosystem, Ecosystem::Repology { .. })
            && !spec::has_unexpanded_macro(project) =>
        {
            let scheme = match ecosystem {
                Ecosystem::Pypi { .. } => Scheme::Pep440,
                _ => Scheme::Generic,
            };
            (ecosystem.tag(), new_version.clone(), scheme)
        }
        _ => return (decision, Vec::new()),
    };
    match verdict(project, &target, scheme, lookups.fetch_repology_latest) {
        Verdict::Agree => (decision, Vec::new()),
        Verdict::PatchLevel { repology } => (
            decision,
            vec![format!(
                "Repology lists {repology}, {source} says {target}: only the patch level differs"
            )],
        ),
        Verdict::Disagrees { repology } => (
            Decision::Skipped(SkipReason::RepologyDisagrees {
                source: source.to_string(),
                target,
                repology,
            }),
            Vec::new(),
        ),
    }
}

#[cfg(test)]
#[path = "repology_tests.rs"]
mod tests;
