//! What a failure needs from the user before a rerun can succeed.
//!
//! A [`Remedy`] names the cause (a missing tool, an unset toolchain, a sandbox
//! that cannot start) and whether the same command is worth running again. It
//! stores no install text: [`install_hint`] builds that from the cause and the
//! host, so the table of package names is the only place they live.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remedy {
    pub cause: Cause,
    pub retry: Retry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cause {
    /// An executable (or an OBS source service, by its name) is not installed.
    /// `path` is where it was looked for, when that is known.
    MissingTool { tool: String, path: Option<PathBuf> },
    /// The tool has no toolchain to run with: none is installed, or none is
    /// set up for it.
    ToolchainUnset { tool: String },
    /// The build sandbox cannot start on this machine. `why` is the reason
    /// as the sandbox reported it.
    SandboxUnavailable { why: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    /// Running the same command again succeeds once the cause is gone.
    SameCommand,
    /// A rerun cannot help until someone acts on the cause.
    None,
}

/// The distribution family of the host, as far as install hints care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostDistro {
    Suse,
    Other,
}

/// The distribution the tool runs on, read from `/etc/os-release`. A host
/// without that file counts as [`HostDistro::Other`].
pub fn host_distro() -> HostDistro {
    std::fs::read_to_string("/etc/os-release")
        .map(|text| parse_os_release(&text))
        .unwrap_or(HostDistro::Other)
}

/// Classify the text of an `os-release` file: `Suse` when the `ID` or
/// `ID_LIKE` value has a word containing `suse` (`opensuse-tumbleweed`,
/// `sles`'s `ID_LIKE="suse"`), `Other` otherwise.
pub fn parse_os_release(text: &str) -> HostDistro {
    let suse = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| matches!(key.trim(), "ID" | "ID_LIKE"))
        .any(|(_, value)| {
            value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .split_whitespace()
                .any(|word| word.contains("suse"))
        });
    if suse {
        HostDistro::Suse
    } else {
        HostDistro::Other
    }
}

/// The install hint for `cause` on `host`, or `None` when there is nothing to
/// suggest. On `Suse` a known package gets `sudo zypper install <package>`.
/// On `Other` the hint names the tool and no command, because the package
/// names here are openSUSE's.
pub fn install_hint(cause: &Cause, host: &HostDistro) -> Option<String> {
    match cause {
        Cause::MissingTool { tool, path } => Some(missing_tool_hint(tool, path.as_deref(), host)),
        Cause::ToolchainUnset { tool } => Some(toolchain_hint(tool)),
        Cause::SandboxUnavailable { .. } => None,
    }
}

fn missing_tool_hint(tool: &str, path: Option<&Path>, host: &HostDistro) -> String {
    let tool = printable(tool);
    match host {
        HostDistro::Suse => match (package_for(&tool), path.and_then(plain_path)) {
            (Some(package), _) => format!("sudo zypper install {package}"),
            (None, Some(path)) => format!("zypper se -f {path}"),
            (None, None) => format!("install the package that provides {tool}"),
        },
        HostDistro::Other => format!("install the package that provides {tool}"),
    }
}

/// The path as text when it is safe to paste into a shell command: only
/// letters, digits and `_ . / + -`. Anything else (a space, a quote, a `;`,
/// a `$`, a backtick, a control character) gives `None`, so no command is
/// suggested for it.
fn plain_path(path: &Path) -> Option<&str> {
    let text = path.to_str()?;
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '+' | '-'));
    plain.then_some(text)
}

/// `text` with every control character (newlines included) replaced by `?`,
/// so a hint is always one line of printable text.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn toolchain_hint(tool: &str) -> String {
    match tool {
        "cargo" => "rustup toolchain install stable".to_string(),
        _ => format!("set a default toolchain for {}", printable(tool)),
    }
}

/// The openSUSE package that ships `tool`. A service name stands for the
/// `obs-service-*` package that ships it; `tar_scm` is shipped by
/// `obs-service-obs_scm-common` (its binary, `/usr/lib/obs/service/tar_scm`),
/// not by `obs-service-tar_scm` (which ships only the unit file).
fn package_for(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "osc" => "osc",
        "rpmspec" => "rpm-build",
        "quilt" => "quilt",
        "bwrap" => "bubblewrap",
        "setpriv" => "util-linux",
        "cargo_vendor" | "cargo_audit" => "obs-service-cargo",
        "tar_scm" => "obs-service-obs_scm-common",
        "obs_scm" => "obs-service-obs_scm",
        "tar" => "obs-service-tar",
        "recompress" => "obs-service-recompress",
        "set_version" => "obs-service-set_version",
        "download_files" => "obs-service-download_files",
        "format_spec_file" => "obs-service-format_spec_file",
        "go_modules" => "obs-service-go_modules",
        "node_modules" => "obs-service-node_modules",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(tool: &str) -> Cause {
        Cause::MissingTool {
            tool: tool.to_string(),
            path: None,
        }
    }

    const TUMBLEWEED: &str = "NAME=\"openSUSE Tumbleweed\"\n# VERSION=\"20261108\"\n\
        ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\nVERSION_ID=\"20261108\"\n";
    const UBUNTU: &str = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";

    #[test]
    fn os_release_with_a_suse_id_is_suse() {
        assert_eq!(parse_os_release(TUMBLEWEED), HostDistro::Suse);
        assert_eq!(
            parse_os_release("ID=sles\nID_LIKE=\"suse\"\n"),
            HostDistro::Suse
        );
        assert_eq!(parse_os_release("ID=suse\n"), HostDistro::Suse);
        assert_eq!(parse_os_release("ID='opensuse-leap'\n"), HostDistro::Suse);
    }

    #[test]
    fn os_release_without_suse_is_other() {
        assert_eq!(parse_os_release(UBUNTU), HostDistro::Other);
        assert_eq!(parse_os_release(""), HostDistro::Other);
        // `suse` in NAME or a pretty name is not the distro family.
        assert_eq!(
            parse_os_release("NAME=\"SUSE Linux\"\nID=fedora\n"),
            HostDistro::Other
        );
    }

    #[test]
    fn every_table_row_on_suse_names_its_package() {
        let rows = [
            ("osc", "osc"),
            ("rpmspec", "rpm-build"),
            ("quilt", "quilt"),
            ("bwrap", "bubblewrap"),
            ("setpriv", "util-linux"),
            ("cargo_vendor", "obs-service-cargo"),
            ("cargo_audit", "obs-service-cargo"),
            ("tar_scm", "obs-service-obs_scm-common"),
            ("obs_scm", "obs-service-obs_scm"),
            ("tar", "obs-service-tar"),
            ("recompress", "obs-service-recompress"),
            ("set_version", "obs-service-set_version"),
            ("download_files", "obs-service-download_files"),
            ("format_spec_file", "obs-service-format_spec_file"),
            ("go_modules", "obs-service-go_modules"),
            ("node_modules", "obs-service-node_modules"),
        ];
        for (tool, package) in rows {
            assert_eq!(
                install_hint(&missing(tool), &HostDistro::Suse).as_deref(),
                Some(format!("sudo zypper install {package}").as_str()),
                "{tool}"
            );
        }
    }

    #[test]
    fn every_table_row_on_other_names_the_tool_without_a_command() {
        for tool in [
            "osc",
            "rpmspec",
            "quilt",
            "bwrap",
            "setpriv",
            "cargo_vendor",
            "cargo_audit",
            "tar_scm",
            "obs_scm",
            "tar",
            "recompress",
            "set_version",
            "download_files",
            "format_spec_file",
            "go_modules",
            "node_modules",
        ] {
            assert_eq!(
                install_hint(&missing(tool), &HostDistro::Other).as_deref(),
                Some(format!("install the package that provides {tool}").as_str()),
                "{tool}"
            );
        }
    }

    #[test]
    fn an_unknown_service_on_suse_is_searched_for_by_its_path() {
        let cause = Cause::MissingTool {
            tool: "new_service".to_string(),
            path: Some(PathBuf::from("/usr/lib/obs/service/new_service")),
        };
        assert_eq!(
            install_hint(&cause, &HostDistro::Suse).as_deref(),
            Some("zypper se -f /usr/lib/obs/service/new_service")
        );
        // Without a path there is nothing to search for: the generic hint.
        assert_eq!(
            install_hint(&missing("new_service"), &HostDistro::Suse).as_deref(),
            Some("install the package that provides new_service")
        );
    }

    #[test]
    fn an_unknown_service_on_other_names_the_tool_even_with_a_path() {
        let cause = Cause::MissingTool {
            tool: "new_service".to_string(),
            path: Some(PathBuf::from("/usr/lib/obs/service/new_service")),
        };
        assert_eq!(
            install_hint(&cause, &HostDistro::Other).as_deref(),
            Some("install the package that provides new_service")
        );
    }

    #[test]
    fn an_unset_cargo_toolchain_is_fixed_by_installing_stable_on_both_hosts() {
        let cause = Cause::ToolchainUnset {
            tool: "cargo".to_string(),
        };
        for host in [HostDistro::Suse, HostDistro::Other] {
            assert_eq!(
                install_hint(&cause, &host).as_deref(),
                Some("rustup toolchain install stable")
            );
        }
        let other = Cause::ToolchainUnset {
            tool: "go".to_string(),
        };
        assert_eq!(
            install_hint(&other, &HostDistro::Suse).as_deref(),
            Some("set a default toolchain for go")
        );
        // The hint never sets a default: the sandbox chooses its own.
        let hint = install_hint(&cause, &HostDistro::Suse).unwrap();
        assert!(!hint.contains("default"), "{hint}");
    }

    #[test]
    fn a_sandbox_that_cannot_start_has_no_install_hint() {
        let cause = Cause::SandboxUnavailable {
            why: "user namespaces are disabled".to_string(),
        };
        assert_eq!(install_hint(&cause, &HostDistro::Suse), None);
        assert_eq!(install_hint(&cause, &HostDistro::Other), None);
    }

    #[test]
    fn a_path_that_is_not_plain_gives_no_command() {
        for path in [
            "/opt/a b/new_service",
            "/opt/a;rm -rf ~",
            "/opt/a\nb",
            "/opt/$(id)/svc",
            "/opt/`id`/svc",
            "/opt/a'b/svc",
            "/opt/a\"b/svc",
            "/opt/a|b",
            "",
        ] {
            let cause = Cause::MissingTool {
                tool: "new_service".to_string(),
                path: Some(PathBuf::from(path)),
            };
            assert_eq!(
                install_hint(&cause, &HostDistro::Suse).as_deref(),
                Some("install the package that provides new_service"),
                "{path:?}"
            );
        }
    }

    #[test]
    fn a_plain_path_with_dots_pluses_and_dashes_keeps_its_command() {
        let cause = Cause::MissingTool {
            tool: "new_service".to_string(),
            path: Some(PathBuf::from("/opt/vendor-1.2+x_y/bin/svc")),
        };
        assert_eq!(
            install_hint(&cause, &HostDistro::Suse).as_deref(),
            Some("zypper se -f /opt/vendor-1.2+x_y/bin/svc")
        );
    }

    #[test]
    fn a_control_character_in_the_tool_is_replaced_in_every_hint() {
        let tool = "bad\nname\x07; rm";
        let missing = Cause::MissingTool {
            tool: tool.to_string(),
            path: None,
        };
        let toolchain = Cause::ToolchainUnset {
            tool: tool.to_string(),
        };
        for host in [HostDistro::Suse, HostDistro::Other] {
            for cause in [&missing, &toolchain] {
                let hint = install_hint(cause, &host).unwrap();
                assert!(!hint.chars().any(char::is_control), "{hint:?}");
                assert!(hint.contains("bad?name?; rm"), "{hint:?}");
            }
        }
    }

    #[test]
    fn a_remedy_round_trips_through_json() {
        let remedy = Remedy {
            cause: Cause::MissingTool {
                tool: "osc".to_string(),
                path: Some(PathBuf::from("/usr/bin/osc")),
            },
            retry: Retry::SameCommand,
        };
        let text = serde_json::to_string(&remedy).unwrap();
        assert_eq!(
            text,
            r#"{"cause":{"kind":"missing_tool","tool":"osc","path":"/usr/bin/osc"},"retry":"same_command"}"#
        );
        let back: Remedy = serde_json::from_str(&text).unwrap();
        assert_eq!(back, remedy);

        let unset = Remedy {
            cause: Cause::ToolchainUnset {
                tool: "cargo".to_string(),
            },
            retry: Retry::None,
        };
        let text = serde_json::to_string(&unset).unwrap();
        assert_eq!(serde_json::from_str::<Remedy>(&text).unwrap(), unset);
    }
}
