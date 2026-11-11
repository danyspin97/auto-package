//! Whether a failed spec check is the machine's fault rather than the spec's.
//! Only a verdict rpmspec gave on the spec goes to the agent. A missing or
//! unlaunchable `rpmspec` stops the package at once: no agent turn can fix it,
//! and a turn that ends "edit kept, check skipped" would loop.

use std::io;

use ap_core::remedy::Remedy;

use crate::pipeline::PipelineResult;
use crate::pipeline::run::RunCtx;

use crate::osc;
use crate::proc::ProcessError;

/// Why a package stops when `rpmspec` is not installed. The install command
/// is not in the text: the remedy carries the cause, and the renderer builds
/// the command from it.
pub(in crate::pipeline) const RPMSPEC_NOT_INSTALLED: &str =
    "rpmspec is not installed (could not launch it)";

/// The message bubblewrap prints when `rpmspec` is not on the sandbox's PATH.
const SANDBOX_NO_RPMSPEC: &str = "execvp rpmspec: No such file or directory";

/// What a failed `rpmspec_check` means for the package.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::pipeline) enum CheckFailure {
    /// rpmspec judged the spec: the agent gets a fix turn.
    Spec,
    /// rpmspec cannot run here: the package stops now with this one-line
    /// reason and, when the cause is a missing tool, its remedy.
    Environment {
        reason: String,
        remedy: Option<Remedy>,
    },
    /// rpmspec timed out or its output was lost: a later run may pass.
    Transient,
}

/// Classify a failed `rpmspec_check`. `tool_on_path` says whether `rpmspec`
/// is an executable on the host's PATH.
pub(in crate::pipeline) fn check_failure(err: &osc::Error, tool_on_path: bool) -> CheckFailure {
    match err {
        osc::Error::Process(ProcessError::Launch { program, source }) => {
            launch_failure(program, source)
        }
        osc::Error::ToolMissing { .. } => CheckFailure::Environment {
            reason: err.to_string(),
            remedy: err.remedy(),
        },
        osc::Error::Process(
            ProcessError::Timeout { .. } | ProcessError::OutputLost { .. } | ProcessError::Io(_),
        ) => CheckFailure::Transient,
        osc::Error::RpmspecFailed { stderr, .. } if stderr.contains(SANDBOX_NO_RPMSPEC) => {
            not_installed()
        }
        // Without rpmspec on PATH the spec is refused for the tool's absence,
        // not for its content.
        osc::Error::UnsandboxedMacro { .. } if !tool_on_path => not_installed(),
        _ => CheckFailure::Spec,
    }
}

/// The failure of a missing `rpmspec`: its reason and the remedy that names
/// the tool.
fn not_installed() -> CheckFailure {
    let missing = osc::Error::ToolMissing {
        tool: "rpmspec".into(),
        path: None,
    };
    CheckFailure::Environment {
        reason: RPMSPEC_NOT_INSTALLED.to_string(),
        remedy: missing.remedy(),
    }
}

/// A launch failure is the environment's. A missing `rpmspec` has its own
/// reason; any other missing program (bubblewrap, say) is a missing tool named
/// by its program. Any other refusal to start a program names the OS error.
fn launch_failure(program: &str, source: &io::Error) -> CheckFailure {
    if source.kind() != io::ErrorKind::NotFound {
        return CheckFailure::Environment {
            reason: format!(
                "{program} could not be started ({source}); fix the local install and run again"
            ),
            remedy: None,
        };
    }
    if program == "rpmspec" {
        return not_installed();
    }
    let missing = osc::Error::ToolMissing {
        tool: program.to_string(),
        path: None,
    };
    CheckFailure::Environment {
        reason: missing.to_string(),
        remedy: missing.remedy(),
    }
}

/// The end of a package whose `rpmspec` check failed on its way to a commit:
/// a verdict on the spec is the spec's error, a check that could not run ends
/// the package as the environment or transient failure it is.
pub(in crate::pipeline) fn publish_check_failed(ctx: &RunCtx, e: &osc::Error) -> PipelineResult {
    match check_failure(e, rpmspec_on_path()) {
        CheckFailure::Environment { reason, remedy } => ctx.failed_env(reason, remedy),
        CheckFailure::Transient => ctx.failed_text(true, e.to_string()),
        CheckFailure::Spec => ctx.failed(format!(
            "the spec does not parse, so it is not committed: {e}"
        )),
    }
}

/// Whether `rpmspec` is an executable in a directory of the host's PATH.
pub(in crate::pipeline) fn rpmspec_on_path() -> bool {
    tool_on_path("rpmspec")
}

pub(in crate::pipeline) use crate::envcheck::tool_on_path;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envcheck::on_path_in;

    use crate::osc;
    use crate::proc::ProcessError;
    use ap_core::remedy::{Cause, Retry};
    use std::io;

    fn launch(kind: io::ErrorKind) -> osc::Error {
        osc::Error::Process(ProcessError::Launch {
            program: "rpmspec".into(),
            source: io::Error::from(kind),
        })
    }

    fn rejected(stderr: &str) -> osc::Error {
        osc::Error::RpmspecFailed {
            spec: "/co/demo.spec".into(),
            code: 1,
            stderr: stderr.into(),
        }
    }

    fn refused(marker: &str) -> osc::Error {
        osc::Error::UnsandboxedMacro {
            spec: "/co/demo.spec".into(),
            marker: marker.into(),
        }
    }

    #[test]
    fn a_missing_rpmspec_stops_the_package_with_the_install_hint() {
        for on_path in [false, true] {
            assert_eq!(
                check_failure(&launch(io::ErrorKind::NotFound), on_path),
                not_installed(),
                "on_path={on_path}"
            );
        }
    }

    #[test]
    fn a_missing_rpmspec_tool_stops_the_package_too() {
        let err = osc::Error::ToolMissing {
            tool: "rpmspec".into(),
            path: None,
        };
        for on_path in [false, true] {
            assert_eq!(
                check_failure(&err, on_path),
                not_installed(),
                "on_path={on_path}"
            );
        }
    }

    #[test]
    fn the_reason_is_one_line_and_names_the_tool_without_an_install_command() {
        assert!(!RPMSPEC_NOT_INSTALLED.contains('\n'));
        assert_eq!(
            RPMSPEC_NOT_INSTALLED,
            "rpmspec is not installed (could not launch it)"
        );
        assert!(!RPMSPEC_NOT_INSTALLED.contains("zypper"));
    }

    #[test]
    fn a_missing_bwrap_is_named_bwrap_not_rpmspec() {
        let err = osc::Error::Process(ProcessError::Launch {
            program: "bwrap".into(),
            source: io::Error::from(io::ErrorKind::NotFound),
        });
        assert_eq!(
            check_failure(&err, true),
            CheckFailure::Environment {
                reason: "bwrap is not installed (could not launch it)".into(),
                remedy: Some(Remedy {
                    cause: Cause::MissingTool {
                        tool: "bwrap".into(),
                        path: None,
                    },
                    retry: Retry::SameCommand,
                }),
            }
        );
    }

    #[test]
    fn a_missing_rpmspec_ends_a_commit_as_an_environment_failure_with_its_remedy() {
        crate::pipeline::review_marks_tests::with_ctx(|ctx, _| {
            let err = osc::Error::ToolMissing {
                tool: "rpmspec".into(),
                path: None,
            };
            match publish_check_failed(ctx, &err) {
                PipelineResult::Failed {
                    kind,
                    reason,
                    remedy,
                    ..
                } => {
                    assert_eq!(kind, crate::pipeline::FailureKind::Environment);
                    assert_eq!(reason, RPMSPEC_NOT_INSTALLED);
                    assert_eq!(remedy, not_installed_remedy());
                }
                _ => panic!("the package did not fail"),
            }
        });
    }

    #[test]
    fn a_missing_rpmspec_carries_a_remedy_for_the_tool() {
        assert_eq!(
            not_installed(),
            CheckFailure::Environment {
                reason: RPMSPEC_NOT_INSTALLED.to_string(),
                remedy: not_installed_remedy(),
            }
        );
    }

    fn not_installed_remedy() -> Option<Remedy> {
        Some(Remedy {
            cause: Cause::MissingTool {
                tool: "rpmspec".into(),
                path: None,
            },
            retry: Retry::SameCommand,
        })
    }

    #[test]
    fn a_launch_refused_for_another_reason_stops_the_package_too() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            match check_failure(&launch(kind), true) {
                CheckFailure::Environment { reason, remedy } => {
                    assert!(
                        reason.starts_with("rpmspec could not be started ("),
                        "{reason}"
                    );
                    assert!(reason.contains("fix the local install"), "{reason}");
                    assert_eq!(remedy, None);
                }
                other => panic!("{kind:?} gave {other:?}"),
            }
        }
    }

    #[test]
    fn a_timeout_or_a_lost_output_is_transient() {
        let timeout = osc::Error::Process(ProcessError::Timeout {
            program: "rpmspec".into(),
            secs: 30,
        });
        let lost = osc::Error::Process(ProcessError::OutputLost {
            program: "rpmspec".into(),
            stream: "stdout",
        });
        for err in [timeout, lost] {
            for on_path in [false, true] {
                assert_eq!(check_failure(&err, on_path), CheckFailure::Transient);
            }
        }
    }

    #[test]
    fn a_read_or_wait_error_on_rpmspec_is_transient() {
        let err = osc::Error::Process(ProcessError::Io(io::Error::other("pipe")));
        assert_eq!(check_failure(&err, true), CheckFailure::Transient);
        assert_eq!(check_failure(&err, false), CheckFailure::Transient);
    }

    #[test]
    fn a_verdict_on_the_spec_goes_to_the_agent() {
        let err = rejected("error: line 3: Unknown tag: Sumary");
        assert_eq!(check_failure(&err, true), CheckFailure::Spec);
        assert_eq!(check_failure(&err, false), CheckFailure::Spec);
    }

    #[test]
    fn bubblewrap_without_rpmspec_on_the_sandbox_path_stops_the_package() {
        let err = rejected("bwrap: execvp rpmspec: No such file or directory\n");
        for on_path in [false, true] {
            assert_eq!(
                check_failure(&err, on_path),
                not_installed(),
                "on_path={on_path}"
            );
        }
    }

    #[test]
    fn a_spec_refused_for_a_macro_with_rpmspec_present_goes_to_the_agent() {
        let err = refused("%(");
        assert_eq!(check_failure(&err, true), CheckFailure::Spec);
        let err = refused("%include");
        assert_eq!(check_failure(&err, true), CheckFailure::Spec);
    }

    #[test]
    fn a_spec_refused_for_a_macro_without_rpmspec_stops_the_package() {
        for marker in ["%(", "%{lua:", "%lua", "%include"] {
            assert_eq!(
                check_failure(&refused(marker), false),
                not_installed(),
                "marker={marker}"
            );
        }
    }

    #[test]
    fn tool_on_path_finds_an_executable_in_a_listed_directory() {
        let with = tempfile::tempdir().unwrap();
        let without = tempfile::tempdir().unwrap();
        let tool = with.path().join("rpmspec");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        make_executable(&tool);
        let path = std::env::join_paths([without.path(), with.path()]).unwrap();
        assert!(on_path_in("rpmspec", &path));
        assert!(!on_path_in("rpmbuild", &path));
        let only_without = std::env::join_paths([without.path()]).unwrap();
        assert!(!on_path_in("rpmspec", &only_without));
    }

    #[test]
    fn tool_on_path_ignores_a_file_that_is_not_executable_and_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rpmspec"), "text\n").unwrap();
        std::fs::create_dir(dir.path().join("rpmbuild")).unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert!(!on_path_in("rpmspec", &path));
        assert!(!on_path_in("rpmbuild", &path));
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}
}
