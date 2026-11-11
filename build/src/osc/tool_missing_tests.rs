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

/// The stderr of a `cargo_vendor` run on a machine with no rustup default, as
/// a user saw it.
const UNSET_RUSTUP: &str = "Error: Custom { kind: Interrupted, error: \"error: rustup could \
    not choose a version of cargo to run, because one wasn't specified explicitly, and no \
    default is configured.\\nhelp: run 'rustup default stable' to download the latest stable \
    release of Rust and set it as your default toolchain.\" }";

fn classified(text: &str) -> Option<String> {
    service_tool_error(text).map(|e| e.to_string())
}

#[test]
fn rustups_no_default_names_the_tool_it_could_not_choose() {
    assert_eq!(
        classified(UNSET_RUSTUP).as_deref(),
        Some("no default toolchain is configured for cargo")
    );
    assert_eq!(
        classified("error: rustup could not choose a version of rustc to run, because"),
        Some("no default toolchain is configured for rustc".to_string())
    );
    assert_eq!(
        classified("rustup: no default is configured for this package"),
        Some("no default toolchain is configured for cargo".to_string())
    );
}

#[test]
fn a_phrase_split_over_two_lines_is_not_an_unset_toolchain() {
    assert_eq!(
        classified("rustup: see the docs\nno default is configured for this package"),
        None
    );
}

#[test]
fn a_missing_rust_tool_is_a_missing_tool_named_by_the_shell() {
    for (text, tool) in [
        ("bash: cargo: command not found", "cargo"),
        ("sh: 1: rustfmt: not found", "rustfmt"),
        ("bwrap: execvp rustc: No such file or directory", "rustc"),
        ("sh: 1: rustup: not found", "rustup"),
    ] {
        assert!(
            matches!(
                service_tool_error(text),
                Some(Error::ToolMissing { tool: t, path: None }) if t == tool
            ),
            "{text}"
        );
    }
}

#[test]
fn a_pinned_toolchain_that_is_not_installed_is_an_unset_toolchain() {
    assert_eq!(
        classified("error: toolchain 'stable-x86_64-unknown-linux-gnu' is not installed"),
        Some("no default toolchain is configured for cargo".to_string())
    );
}

#[test]
fn a_line_that_only_mentions_rustup_or_another_path_is_not_a_tool_error() {
    for text in [
        "   Compiling rustup-toolchain v0.3.1 (uses rustup for the default toolchain)",
        "note: run 'rustup default stable' to set up a toolchain",
        "error: could not run /usr/bin/cargo-audit: No such file or directory",
        "error: could not compile `demo` (lib)",
        "",
    ] {
        assert!(service_tool_error(text).is_none(), "{text}");
    }
}

#[test]
fn an_unset_toolchain_carries_a_same_command_remedy_and_is_not_transient() {
    let err = Error::ToolchainUnset {
        tool: "cargo".into(),
    };
    assert_eq!(
        err.remedy(),
        Some(Remedy {
            cause: Cause::ToolchainUnset {
                tool: "cargo".into(),
            },
            retry: Retry::SameCommand,
        })
    );
    assert!(!err.is_transient());
    assert_eq!(
        err.to_string(),
        "no default toolchain is configured for cargo"
    );
}

#[test]
fn a_failed_service_run_with_rustups_text_is_an_unset_toolchain() {
    let err = super::service::service_run_failed(
        std::path::Path::new("/co"),
        1,
        UNSET_RUSTUP.to_string(),
    );
    assert!(matches!(err, Error::ToolchainUnset { tool } if tool == "cargo"));
}

#[test]
fn any_other_failed_service_run_keeps_its_text() {
    let err = super::service::service_run_failed(
        std::path::Path::new("/co"),
        3,
        "service cargo_vendor failed:\nboom".to_string(),
    );
    match err {
        Error::OscFailed { cmd, code, stderr } => {
            assert_eq!(cmd, "service ra in /co");
            assert_eq!(code, 3);
            assert_eq!(stderr, "service cargo_vendor failed:\nboom");
        }
        other => panic!("{other}"),
    }
}
