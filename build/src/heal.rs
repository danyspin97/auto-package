//! Start-up healing of what `state.json`, a package's checkout and OBS say
//! about each other. A fix the tool can make safely is made here and recorded
//! in the package's `reconciled.log`; what only a person can settle is left to
//! the self-check in `doctor`, which says the one thing to do. The decisions
//! are pure: the callers read the files and ask OBS, then act on the answer.

use crate::osc::BuildStatus;

/// What to do with a file the publish plan leaves out of a checkout.
#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveAction {
    /// Delete the local copy: the tool's own scratch of another release.
    Delete,
    /// Leave it alone.
    Keep,
}

/// Whether a file the publish plan skipped is a leftover archive of another
/// release that the tool may delete from the checkout. That is an archive the
/// spec does not name, that OBS does not track, and whose name does not carry
/// the spec's version. An archive OBS tracks is never deleted here: the next
/// commit removes it from OBS on purpose, as a version bump does.
pub fn stale_archive_action(
    file: &str,
    is_archive: bool,
    tracked: bool,
    named: bool,
    spec_version: &str,
) -> ArchiveAction {
    if is_archive && !tracked && !named && !crate::doctor::names_version(file, spec_version) {
        ArchiveAction::Delete
    } else {
        ArchiveAction::Keep
    }
}

/// Whether a recorded review is of a version other than the spec's. It does
/// not apply to the spec as it is, so it is dropped; the next run reviews the
/// spec again, which costs one review at most.
pub fn is_stale_review(reviewed: Option<&str>, spec_version: Option<&str>) -> bool {
    matches!((reviewed, spec_version), (Some(r), Some(v)) if r != v)
}

/// What the OBS comparison of one package decides.
#[derive(Debug, PartialEq, Eq)]
pub enum ObsDecision {
    /// Not a package OBS built while the state says it did not: nothing to do.
    NotApplicable,
    /// OBS built the version the spec says from a clean checkout: the RPMs are
    /// fetched and the state recorded as built.
    Fetch,
    /// OBS built it, but the tool cannot take the RPMs safely. The text says
    /// the one thing to do.
    Decline(String),
}

/// Decide what the start-up reconcile does for a package whose state says it
/// did not build and whose OBS status is `obs`. Only a state of `failed`,
/// `interrupted` or `missing_deps` with OBS `succeeded` applies. The version
/// the state last built must be the spec's, and the checkout must have no local
/// changes (`dirty` is asked last, as it runs `osc status`).
pub fn obs_built_decision(
    outcome: Option<&str>,
    obs: Option<&BuildStatus>,
    spec_version: Option<&str>,
    last_built: Option<&str>,
    dirty: impl FnOnce() -> bool,
) -> ObsDecision {
    let applies = matches!(outcome, Some("failed" | "interrupted" | "missing_deps"))
        && obs == Some(&BuildStatus::Succeeded);
    if !applies {
        return ObsDecision::NotApplicable;
    }
    let Some(spec) = spec_version else {
        return ObsDecision::Decline(
            "its spec cannot be read in the checkout; restore the checkout, then run update for it"
                .to_string(),
        );
    };
    if last_built != Some(spec) {
        return ObsDecision::Decline(format!(
            "state.json says the last build was {} and the spec says {spec}; run update for it \
             to build {spec}",
            last_built.unwrap_or("nothing")
        ));
    }
    if dirty() {
        return ObsDecision::Decline(
            "the checkout has local changes; discard or commit them, and the next run takes \
             the RPMs"
                .to_string(),
        );
    }
    ObsDecision::Fetch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leftover_archive_of_another_release_is_deleted_only_when_nothing_owns_it() {
        let stale = |tracked, named| {
            stale_archive_action("pyenv-2.8.4.tar.gz", true, tracked, named, "2.8.14")
        };
        assert_eq!(stale(false, false), ArchiveAction::Delete);
        // OBS holds it: the next commit removes it from OBS, not the reconcile.
        assert_eq!(stale(true, false), ArchiveAction::Keep);
        // The spec names it.
        assert_eq!(stale(false, true), ArchiveAction::Keep);
        // Not an archive: the user's file, left alone.
        assert_eq!(
            stale_archive_action("notes.txt", false, false, false, "2.8.14"),
            ArchiveAction::Keep
        );
        // The spec's own version is not an old release.
        assert_eq!(
            stale_archive_action("pyenv-2.8.14.tar.gz", true, false, false, "2.8.14"),
            ArchiveAction::Keep
        );
    }

    #[test]
    fn a_review_of_another_version_is_stale_and_one_of_the_spec_is_not() {
        assert!(is_stale_review(Some("0.7.12"), Some("0.7.11")));
        assert!(!is_stale_review(Some("0.7.11"), Some("0.7.11")));
        assert!(!is_stale_review(None, Some("0.7.11")));
        assert!(!is_stale_review(Some("0.7.12"), None));
    }

    #[test]
    fn only_a_failed_state_that_obs_built_is_decided() {
        let dirty = || panic!("not asked for this outcome");
        for outcome in [None, Some("success"), Some("skipped"), Some("running")] {
            assert_eq!(
                obs_built_decision(
                    outcome,
                    Some(&BuildStatus::Succeeded),
                    Some("1.0"),
                    Some("1.0"),
                    dirty
                ),
                ObsDecision::NotApplicable,
                "{outcome:?}"
            );
        }
        assert_eq!(
            obs_built_decision(
                Some("failed"),
                Some(&BuildStatus::Failed),
                Some("1.0"),
                Some("1.0"),
                dirty
            ),
            ObsDecision::NotApplicable
        );
    }

    #[test]
    fn a_clean_checkout_at_the_recorded_version_is_fetched() {
        for outcome in ["failed", "interrupted", "missing_deps"] {
            assert_eq!(
                obs_built_decision(
                    Some(outcome),
                    Some(&BuildStatus::Succeeded),
                    Some("2.7.1"),
                    Some("2.7.1"),
                    || false
                ),
                ObsDecision::Fetch,
                "{outcome}"
            );
        }
    }

    #[test]
    fn a_checkout_with_changes_or_another_version_is_declined_with_the_one_thing_to_do() {
        let built = Some(&BuildStatus::Succeeded);
        let dirty = obs_built_decision(Some("failed"), built, Some("2.0"), Some("2.0"), || true);
        assert!(
            matches!(&dirty, ObsDecision::Decline(r) if r.contains("discard or commit")),
            "{dirty:?}"
        );
        let other = obs_built_decision(Some("failed"), built, Some("2.0"), Some("1.0"), || false);
        assert!(
            matches!(&other, ObsDecision::Decline(r) if r.contains("the spec says 2.0") && r.contains("run update")),
            "{other:?}"
        );
        let unread = obs_built_decision(Some("failed"), built, None, Some("1.0"), || false);
        assert!(matches!(unread, ObsDecision::Decline(_)), "{unread:?}");
        // A state that never built says so rather than `None`.
        let never = obs_built_decision(Some("failed"), built, Some("2.0"), None, || false);
        assert!(
            matches!(&never, ObsDecision::Decline(r) if r.contains("nothing")),
            "{never:?}"
        );
    }
}
