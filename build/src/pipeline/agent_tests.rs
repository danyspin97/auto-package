use super::*;

/// A turn rejected for a change to the checkout's metadata, with the change
/// that could not be put back named in the detail.
fn tampered(detail: &str) -> crate::agent::Error {
    crate::agent::Error::Tampered {
        detail: detail.into(),
        usage: crate::agent::Usage::default(),
    }
}

#[test]
fn a_rejected_metadata_change_is_its_own_failure_kind() {
    // Not `AgentFailed`: that kind is downgraded to a warning on a style fix.
    let kind = turn_failure_kind(&tampered(
        "modified osc/git metadata of the checkout (modified .osc/_apiurl); \
         could not restore .osc/_apiurl: its contents were not kept",
    ));
    assert_eq!(kind, FailureKind::Tampered);
}

#[test]
fn a_turn_that_failed_on_its_own_stays_agent_failed() {
    let kind = turn_failure_kind(&crate::agent::Error::Io(std::io::Error::other("stream")));
    assert_eq!(kind, FailureKind::AgentFailed);
}
