//! What a branch project already holds for a package that `update` is about to
//! branch. It is read before the branch call, so a branch that holds the
//! upstream version is not rebuilt, and one that holds a newer version than
//! upstream's is not replaced by it.

use crate::proc::Emit;
use crate::spec;
use crate::update;
use crate::version::{self, Scheme};

/// What `update` does with a package, given the version its branch holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchState {
    /// The branch does not hold the package, or its spec cannot be read: the
    /// package is branched as it always is.
    Fresh,
    /// The branch holds an older version than upstream's, or one that cannot
    /// be compared: the update goes on from the branch.
    Continue,
    /// The branch holds upstream's version already. It is not rebuilt, but it
    /// is listed, so that its submit request can still be made.
    AlreadyUpdated,
    /// The branch holds a version newer than upstream's, and the update is an
    /// upgrade of the origin. The package is skipped and flagged.
    AheadOfUpstream { branch: String },
}

/// Decide from the origin's version, the version the branch holds (`None`
/// when it does not hold the package) and upstream's version. The update is
/// an upgrade when upstream is newer than the origin; a downgrade has no
/// newer branch to protect, so only an equal branch is recognised there.
pub fn branch_state(
    origin: &str,
    branch: Option<&str>,
    upstream: &str,
    scheme: Scheme,
) -> BranchState {
    let Some(branch) = branch else {
        return BranchState::Fresh;
    };
    // The very same version is the same package, snapshot or not.
    if branch == upstream {
        return BranchState::AlreadyUpdated;
    }
    // A snapshot or a computed version is not a release: it cannot say what
    // upstream's release is, so the update goes on as it did.
    if version::is_snapshot(branch) || branch.contains('%') {
        return BranchState::Continue;
    }
    match version::cmp(branch, upstream, scheme) {
        Some(std::cmp::Ordering::Equal) => BranchState::AlreadyUpdated,
        Some(std::cmp::Ordering::Greater)
            if version::cmp(upstream, origin, scheme) == Some(std::cmp::Ordering::Greater) =>
        {
            BranchState::AheadOfUpstream {
                branch: branch.to_string(),
            }
        }
        _ => BranchState::Continue,
    }
}

/// Whether the branch holds a version newer than its origin's: a `lint` of
/// the package then continues from the branch (an earlier update), instead of
/// rebasing it onto the origin. A snapshot is a version like any other here
/// (`1.3+git20260101` is newer than `1.0`). A branch equal to the origin or
/// behind it is not ahead, and neither is one whose version cannot be compared
/// (see [`branch_incomparable`]).
/// [`branch_state`] cannot say this: its ahead case needs an upstream newer
/// than the origin, and a lint has none.
pub fn branch_ahead_of_origin(origin: &str, branch: &str, scheme: Scheme) -> bool {
    !branch.contains('%')
        && !origin.contains('%')
        && version::cmp(branch, origin, scheme) == Some(std::cmp::Ordering::Greater)
}

/// Whether the branch differs from its origin and the two cannot be ordered (a
/// macro such as `%{upstream}` left in a Version, or a string that is not a
/// version). Such a branch is neither ahead nor behind: a lint leaves it alone
/// with a warning, because a rebase onto the origin could discard its update.
pub fn branch_incomparable(origin: &str, branch: &str, scheme: Scheme) -> bool {
    branch != origin
        && (branch.contains('%')
            || origin.contains('%')
            || version::cmp(branch, origin, scheme).is_none())
}

/// The `Version:` of `pkg` in `branch`, read from the branch's spec. `Ok(None)`
/// when the branch does not hold the package; `Err` when it could not be read
/// or its spec has no header, which says nothing about what the branch holds.
pub fn branch_version(branch: &str, pkg: &str, emit: Emit<'_>) -> Result<Option<String>, String> {
    version_of_read(update::spec_for_package(branch, pkg, emit))
}

/// [`branch_version`] given the read of the branch's spec. A missing spec, and
/// a package the branch does not have ([`update::ReadError::Absent`]: OBS
/// answered 404), are the branch not holding the package.
fn version_of_read(read: Result<String, update::ReadError>) -> Result<Option<String>, String> {
    match read {
        // A spec that has no readable header is not a missing package: it is
        // there, and what it holds is unknown.
        Ok(text) => match spec::parse_header(&text) {
            Some(h) => Ok(Some(h.version)),
            None => Err("its spec has no readable Name: and Version:".to_string()),
        },
        Err(update::ReadError::NoSpec | update::ReadError::Absent(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(origin: &str, branch: Option<&str>, upstream: &str) -> BranchState {
        branch_state(origin, branch, upstream, Scheme::Generic)
    }

    #[test]
    fn a_package_the_branch_does_not_hold_is_branched_as_before() {
        assert_eq!(state("1.0", None, "1.1"), BranchState::Fresh);
    }

    #[test]
    fn a_branch_at_upstream_is_already_updated() {
        assert_eq!(
            state("1.0", Some("1.1"), "1.1"),
            BranchState::AlreadyUpdated
        );
    }

    #[test]
    fn a_branch_between_origin_and_upstream_continues_from_it() {
        assert_eq!(state("1.0", Some("1.0"), "1.2"), BranchState::Continue);
        assert_eq!(state("1.0", Some("1.1"), "1.2"), BranchState::Continue);
    }

    #[test]
    fn a_branch_newer_than_upstream_is_ahead_of_it() {
        assert_eq!(
            state("1.0", Some("1.3"), "1.2"),
            BranchState::AheadOfUpstream {
                branch: "1.3".to_string()
            }
        );
    }

    #[test]
    fn a_downgrade_is_not_held_back_by_a_newer_branch() {
        // Origin 2.0 is being brought back to 1.5: a branch on 1.8 is not
        // ahead of anything the update should keep.
        assert_eq!(state("2.0", Some("1.8"), "1.5"), BranchState::Continue);
        assert_eq!(state("2.0", Some("2.0"), "1.5"), BranchState::Continue);
    }

    #[test]
    fn a_snapshot_or_computed_branch_version_is_not_compared() {
        assert_eq!(
            state("1.0", Some("1.3+git20260101.abc123"), "1.2"),
            BranchState::Continue
        );
        assert_eq!(
            state("1.0", Some("1.3.%(get_version.sh)"), "1.2"),
            BranchState::Continue
        );
    }

    #[test]
    fn a_version_that_cannot_be_compared_continues() {
        assert_eq!(
            state("1.0", Some("%{version}"), "1.2"),
            BranchState::Continue
        );
    }

    #[test]
    fn the_same_snapshot_as_upstream_is_already_updated() {
        assert_eq!(
            state(
                "1.0",
                Some("1.3+git20260101.abc123"),
                "1.3+git20260101.abc123"
            ),
            BranchState::AlreadyUpdated
        );
    }

    #[test]
    fn a_missing_spec_or_package_is_a_branch_that_does_not_hold_it() {
        use update::ReadError;
        assert_eq!(version_of_read(Err(ReadError::NoSpec)), Ok(None));
        // OBS's 404 for the package: the branch is not branched yet, so the
        // package goes down the branch path, with no "left alone" skip.
        assert_eq!(
            version_of_read(Err(ReadError::Absent(
                "foo does not exist on OBS (Server returned an error: HTTP Error 404: Not Found); \
                 not retried"
                    .into()
            ))),
            Ok(None)
        );
    }

    #[test]
    fn a_failed_read_with_the_404_text_is_not_a_missing_branch() {
        // Only OBS's permanent 404 classifies a package as absent: a failed read
        // that merely mentions it says nothing about the branch.
        use update::ReadError;
        assert!(
            version_of_read(Err(ReadError::Failed(
                "foo does not exist on OBS (transient)".into()
            )))
            .is_err()
        );
    }

    #[test]
    fn a_read_that_failed_says_nothing_about_the_branch() {
        use update::ReadError;
        assert!(
            version_of_read(Err(ReadError::Failed(
                "listing the package's files (osc ls) failed: timeout, 3 attempts".into()
            )))
            .is_err()
        );
        assert!(version_of_read(Err(ReadError::NeedsMaintainer("broken link".into()))).is_err());
    }

    #[test]
    fn a_spec_without_a_header_says_nothing_about_the_branch() {
        assert!(version_of_read(Ok("%description\nnothing\n".to_string())).is_err());
    }

    #[test]
    fn a_readable_branch_gives_its_version() {
        let spec = "Name:           foo\nVersion:        1.2\nRelease:        0\n";
        assert_eq!(
            version_of_read(Ok(spec.to_string())),
            Ok(Some("1.2".to_string()))
        );
    }

    #[test]
    fn a_branch_is_ahead_of_its_origin_only_when_newer() {
        let ahead = |o, b| branch_ahead_of_origin(o, b, Scheme::Generic);
        assert!(ahead("5.3", "5.4"));
        assert!(!ahead("5.3", "5.3"));
        assert!(!ahead("5.3", "5.2"));
        assert!(!ahead("5.3", "%{version}"));
        assert!(!ahead("%{version}", "5.4"));
    }

    #[test]
    fn a_snapshot_newer_than_the_origin_is_ahead() {
        let ahead = |o, b| branch_ahead_of_origin(o, b, Scheme::Generic);
        assert!(ahead("1.0", "1.3+git20260101.abc123"));
        assert!(!ahead("1.3", "1.2+git20260101.abc123"));
    }

    #[test]
    fn a_version_that_cannot_be_compared_is_incomparable() {
        let incomparable = |o, b| branch_incomparable(o, b, Scheme::Generic);
        assert!(incomparable("5.3", "%{version}"));
        assert!(incomparable("5.3", "not a version"));
        assert!(!incomparable("5.3", "5.3"));
        assert!(!incomparable("5.3", "5.4"));
        assert!(!incomparable("5.3", "5.2"));
    }
}
