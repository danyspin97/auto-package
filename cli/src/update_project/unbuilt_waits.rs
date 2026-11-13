//! What a package is told when an update it waited for was not built (see
//! [`super::same_wait`]).
//!
//! A package waits for the build of each update it requires (its job's
//! `after`), and the wait ends whether the build succeeds or not. A failed
//! update's branch then hides the source's binaries from the branch project,
//! so the package is built there against a dependency that has no build: it
//! may be unresolvable. Nothing else says so, so the run warns once per such
//! package.

use super::*;

/// One warning per package whose required update did not build, given the
/// run's `results` (after the builds are merged). A required update is one of
/// the package's `requires`, matched by package name.
pub(super) fn unbuilt_waits(results: &[ItemResult]) -> Vec<String> {
    let mut lines = Vec::new();
    for result in results {
        for required in &result.item.requires {
            let dep = results
                .iter()
                .find(|r| osc::fuzzy_key(&r.item.pkg) == osc::fuzzy_key(&required.pkg));
            let Some(state) = dep.and_then(|d| unbuilt_state(&d.outcome)) else {
                continue;
            };
            lines.push(format!(
                "{}: waited for the update of {}, which {state}; {} is built in the branch \
                 project anyway, where {} has no build of the update, so it may be unresolvable",
                result.item.pkg, required.pkg, result.item.pkg, required.pkg
            ));
        }
    }
    lines
}

/// How an update's outcome says it was not built, `None` when it was (built
/// or already in the branch).
fn unbuilt_state(outcome: &ItemOutcome) -> Option<String> {
    match outcome {
        ItemOutcome::Updated | ItemOutcome::AlreadyUpdated => None,
        ItemOutcome::Failed(why) => Some(format!("failed ({why})")),
        ItemOutcome::Skipped(why) => Some(format!("was skipped ({why})")),
        ItemOutcome::NotStarted => Some("was not started".to_string()),
        ItemOutcome::Interrupted => Some("was interrupted".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_build::update_project::RequiredUpdate;

    fn result(pkg: &str, requires: &[&str], outcome: ItemOutcome) -> ItemResult {
        let mut item = Item {
            pkg: pkg.into(),
            origin: "devel:languages:python".into(),
            ecosystem: ap_build::update::Ecosystem::GitRepo {
                url: format!("https://github.com/a/{pkg}"),
            },
            old_version: "1".into(),
            new_version: "2".into(),
            upstream_tag: None,
            held_version: None,
            dependency_of: None,
            requires: Vec::new(),
            findings: Vec::new(),
        };
        for dep in requires {
            item.requires.push(RequiredUpdate {
                pkg: (*dep).into(),
                branch_package: format!("home:u:branches:devel:languages:python/{dep}"),
                requirement: format!("{dep}-devel >= 2"),
                origin: "devel:languages:python".into(),
                origin_version: "1".into(),
                request: None,
            });
        }
        ItemResult {
            item,
            outcome,
            sr: SrOutcome::NotRequested,
        }
    }

    #[test]
    fn a_waited_update_that_failed_is_said_for_the_package_that_waited() {
        let results = [
            result("libb", &[], ItemOutcome::Failed("build failed".into())),
            result("liba", &["libb"], ItemOutcome::Updated),
        ];
        assert_eq!(
            unbuilt_waits(&results),
            vec![
                "liba: waited for the update of libb, which failed (build failed); liba is \
                 built in the branch project anyway, where libb has no build of the update, so \
                 it may be unresolvable"
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_waited_update_that_built_or_was_already_there_is_not_said() {
        let results = [
            result("libb", &[], ItemOutcome::Updated),
            result("libc", &[], ItemOutcome::AlreadyUpdated),
            result("liba", &["libb", "libc"], ItemOutcome::Updated),
        ];
        assert!(unbuilt_waits(&results).is_empty());
    }

    #[test]
    fn a_waited_update_not_started_or_skipped_is_said_too() {
        let results = [
            result("libb", &[], ItemOutcome::NotStarted),
            result("libc", &[], ItemOutcome::Skipped("held".into())),
            result("liba", &["libb", "libc"], ItemOutcome::NotStarted),
        ];
        let lines = unbuilt_waits(&results);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("waited for the update of libb, which was not started"));
        assert!(lines[1].contains("libc, which was skipped (held)"));
    }
}
