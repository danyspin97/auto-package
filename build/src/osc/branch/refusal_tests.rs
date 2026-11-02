use super::*;
use crate::osc::error::*;

#[test]
fn a_branch_refused_with_422_says_so_and_how_to_see_why() {
    let out = crate::proc::Captured {
        code: 1,
        stdout: String::new(),
        stderr: "Server returned an error: HTTP Error 422: Unprocessable Entity\n".into(),
    };
    let err = branch_refusal(
        "X11:Wayland",
        "drmcru",
        "home:u:branches:X11:Wayland",
        "/source/X11:Wayland/drmcru?cmd=branch",
        &out,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("OBS refused the request"), "{msg}");
    assert!(msg.contains("HTTP Error 422"), "{msg}");
    assert!(msg.contains("the same `osc branch` with `-d`"), "{msg}");
    assert!(!msg.contains("-X POST"), "{msg}");
    assert!(!matches!(err, Error::OscFailed { .. }), "{msg}");
}

#[test]
fn a_branch_that_already_exists_is_reused_not_refused() {
    let out = crate::proc::Captured {
        code: 1,
        stdout: String::new(),
        stderr: "<status code=\"double_branch_package\">x</status>".into(),
    };
    assert!(matches!(
        branch_refusal("S", "p", "T", "/source/S/p?cmd=branch", &out),
        Ok(BranchResult::AlreadyThere)
    ));
}
