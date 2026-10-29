use super::*;

#[test]
fn a_transient_listing_failure_stays_retryable() {
    let timeout = Error::Command {
        cmd: "ls project".into(),
        source: proc::ProcessError::Timeout {
            program: "osc".into(),
            secs: 180,
        },
    };
    assert!(listing_failure_for_create("project", timeout).is_transient());

    let bad_gateway = Error::OscFailed {
        cmd: "ls project".into(),
        code: 1,
        stderr: "HTTP Error 502: Bad Gateway".into(),
    };
    assert!(listing_failure_for_create("project", bad_gateway).is_transient());
}

#[test]
fn a_permanent_listing_failure_is_not_actionable() {
    let refused = Error::OscFailed {
        cmd: "ls project".into(),
        code: 1,
        stderr: "HTTP Error 403: Forbidden".into(),
    };
    match listing_failure_for_create("project", refused) {
        Error::NotActionable(m) => assert!(m.contains("could not list packages of project")),
        other => panic!("expected NotActionable, got {other:?}"),
    }
    let invalid = Error::NotActionable("refusing to use project".into());
    assert!(!listing_failure_for_create("project", invalid).is_transient());
}
