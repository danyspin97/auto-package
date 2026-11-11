use super::*;
use ap_core::remedy::{Cause, Remedy, Retry};

fn missing(tool: &str, path: Option<&str>) -> Error {
    Error::ToolMissing {
        tool: tool.into(),
        path: path.map(PathBuf::from),
    }
}

#[test]
fn a_missing_service_names_the_tool_and_its_path_without_an_install_command() {
    let err = missing("cargo_vendor", Some("/usr/lib/obs/service/cargo_vendor"));
    assert_eq!(
        err.to_string(),
        "cargo_vendor is not installed (/usr/lib/obs/service/cargo_vendor is missing)"
    );
    assert!(!err.to_string().contains("zypper"));
}

#[test]
fn a_tool_that_would_not_launch_says_so_without_a_path() {
    let err = missing("rpmspec", None);
    assert_eq!(
        err.to_string(),
        "rpmspec is not installed (could not launch it)"
    );
}

#[test]
fn a_missing_tool_carries_a_same_command_remedy_with_its_cause() {
    assert_eq!(
        missing("rpmspec", None).remedy(),
        Some(Remedy {
            cause: Cause::MissingTool {
                tool: "rpmspec".into(),
                path: None,
            },
            retry: Retry::SameCommand,
        })
    );
    assert_eq!(
        missing("tar_scm", Some("/usr/lib/obs/service/tar_scm")).remedy(),
        Some(Remedy {
            cause: Cause::MissingTool {
                tool: "tar_scm".into(),
                path: Some(PathBuf::from("/usr/lib/obs/service/tar_scm")),
            },
            retry: Retry::SameCommand,
        })
    );
}

#[test]
fn a_missing_tool_is_never_transient() {
    assert!(!missing("rpmspec", None).is_transient());
    assert!(!missing("cargo_vendor", Some("/x/cargo_vendor")).is_transient());
}

#[test]
fn other_errors_have_no_remedy() {
    let failed = Error::OscFailed {
        cmd: "x".into(),
        code: 1,
        stderr: "boom".into(),
    };
    assert_eq!(failed.remedy(), None);
    assert_eq!(
        Error::ServiceSandbox("bwrap is not installed".into()).remedy(),
        None
    );
}
