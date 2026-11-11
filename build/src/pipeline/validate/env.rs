//! Whether a failed spec check is the machine's fault rather than the spec's.
//! Only a verdict rpmspec gave on the spec goes to the agent. A missing or
//! unlaunchable `rpmspec` stops the package at once: no agent turn can fix it,
//! and a turn that ends "edit kept, check skipped" would loop.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use crate::osc;
use crate::proc::ProcessError;

/// Why a package stops when `rpmspec` is not installed.
pub(in crate::pipeline) const RPMSPEC_NOT_INSTALLED: &str = "rpmspec is not installed (could not launch it); install rpm-build (sudo zypper install rpm-build) and run again";

/// The message bubblewrap prints when `rpmspec` is not on the sandbox's PATH.
const SANDBOX_NO_RPMSPEC: &str = "execvp rpmspec: No such file or directory";

/// What a failed `rpmspec_check` means for the package.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::pipeline) enum CheckFailure {
    /// rpmspec judged the spec: the agent gets a fix turn.
    Spec,
    /// rpmspec cannot run here: the package stops now with this one-line reason.
    Environment(String),
    /// rpmspec timed out or its output was lost: a later run may pass.
    Transient,
}

/// Classify a failed `rpmspec_check`. `tool_on_path` says whether `rpmspec`
/// is an executable on the host's PATH.
pub(in crate::pipeline) fn check_failure(err: &osc::Error, tool_on_path: bool) -> CheckFailure {
    match err {
        osc::Error::Process(ProcessError::Launch { source, .. }) => launch_failure(source),
        osc::Error::ToolMissing { .. } => {
            CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string())
        }
        osc::Error::Process(
            ProcessError::Timeout { .. } | ProcessError::OutputLost { .. } | ProcessError::Io(_),
        ) => CheckFailure::Transient,
        osc::Error::RpmspecFailed { stderr, .. } if stderr.contains(SANDBOX_NO_RPMSPEC) => {
            CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string())
        }
        // Without rpmspec on PATH the spec is refused for the tool's absence,
        // not for its content.
        osc::Error::UnsandboxedMacro { .. } if !tool_on_path => {
            CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string())
        }
        _ => CheckFailure::Spec,
    }
}

/// A launch failure of `rpmspec` is the environment's: a missing program has
/// its own reason, any other refusal to start it names the OS error.
fn launch_failure(source: &io::Error) -> CheckFailure {
    if source.kind() == io::ErrorKind::NotFound {
        CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string())
    } else {
        CheckFailure::Environment(format!(
            "rpmspec could not be started ({source}); fix the local install and run again"
        ))
    }
}

/// Whether `rpmspec` is an executable in a directory of the host's PATH.
pub(in crate::pipeline) fn rpmspec_on_path() -> bool {
    tool_on_path("rpmspec")
}

/// Whether `name` is an executable file in a directory of the host's PATH.
pub(in crate::pipeline) fn tool_on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| on_path_in(name, &path))
}

pub(in crate::pipeline) fn on_path_in(name: &str, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|dir| is_executable(&dir.join(name)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::osc;
    use crate::proc::ProcessError;
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
                CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string()),
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
                CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string()),
                "on_path={on_path}"
            );
        }
    }

    #[test]
    fn the_reason_is_one_line_and_names_the_package_to_install() {
        assert!(!RPMSPEC_NOT_INSTALLED.contains('\n'));
        assert_eq!(
            RPMSPEC_NOT_INSTALLED,
            "rpmspec is not installed (could not launch it); install rpm-build \
             (sudo zypper install rpm-build) and run again"
        );
    }

    #[test]
    fn a_launch_refused_for_another_reason_stops_the_package_too() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            match check_failure(&launch(kind), true) {
                CheckFailure::Environment(reason) => {
                    assert!(
                        reason.starts_with("rpmspec could not be started ("),
                        "{reason}"
                    );
                    assert!(reason.contains("fix the local install"), "{reason}");
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
                CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string()),
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
                CheckFailure::Environment(RPMSPEC_NOT_INSTALLED.to_string()),
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
