//! How each check is detected. Every function reads a [`Probe`] and returns
//! an [`Outcome`]; only `Probe::run` starts a process. The rustup rows reuse
//! the sandbox's own functions, so they report what the sandbox will do.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::Duration;

use ap_core::remedy::{Cause, printable};

use super::{Outcome, Probe};
use crate::agent::{NOT_LOGGED_IN_MESSAGE, status_says_logged_out};
use crate::service_sandbox::{
    Choice, Inputs, RustupHome, UNSAFE_ENV, host_default, installed_match, installed_toolchains,
    resolve_rustup_home, select_toolchain,
};

/// The agent's start check gives `claude auth status` 30 seconds.
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(30);
const WHOIS_TIMEOUT: Duration = Duration::from_secs(10);
const SUDO_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether `name` is an executable file in a directory of the host's PATH.
pub fn tool_on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| on_path_in(name, &path))
}

/// Whether `name` is an executable file in one of the directories of `path`.
pub fn on_path_in(name: &str, path: &OsStr) -> bool {
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

fn missing_tool(name: &str) -> Cause {
    Cause::MissingTool {
        tool: name.to_string(),
        path: None,
    }
}

fn tool(probe: &Probe<'_>, name: &str) -> Outcome {
    if on_path_in(name, &probe.path) {
        Outcome::Ok(format!("{name} is on PATH"))
    } else {
        Outcome::Missing(missing_tool(name))
    }
}

fn has_value(probe: &Probe<'_>, key: &str) -> bool {
    probe.env.get(key).is_some_and(|v| !v.trim().is_empty())
}

pub fn osc(probe: &Probe<'_>) -> Outcome {
    tool(probe, "osc")
}

pub fn rpmspec(probe: &Probe<'_>) -> Outcome {
    tool(probe, "rpmspec")
}

pub fn quilt(probe: &Probe<'_>) -> Outcome {
    tool(probe, "quilt")
}

/// osc's `build` script, which `osc build` runs for `--local`.
pub fn build_script(probe: &Probe<'_>) -> Outcome {
    tool(probe, "build")
}

pub fn gpg(probe: &Probe<'_>) -> Outcome {
    if on_path_in("gpg", &probe.path) {
        Outcome::Ok("gpg is on PATH".to_string())
    } else {
        Outcome::Warn(missing_tool("gpg"))
    }
}

/// No host spawn of a C compiler exists in auto-package: this is advice for
/// the packages a local build may pull in.
pub fn cc(probe: &Probe<'_>) -> Outcome {
    if on_path_in("gcc", &probe.path) || on_path_in("cc", &probe.path) {
        Outcome::Ok("gcc or cc is on PATH".to_string())
    } else {
        Outcome::Warn(missing_tool("gcc"))
    }
}

pub fn oscrc(probe: &Probe<'_>) -> Outcome {
    let found = probe
        .oscrc_paths
        .iter()
        .find(|path| path.is_file() && fs::File::open(path).is_ok());
    match found {
        Some(path) => Outcome::Ok(format!("oscrc at {}", path.display())),
        None => {
            let looked: Vec<String> = probe
                .oscrc_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            Outcome::MissingText(format!(
                "no readable oscrc (looked at {}): run osc once to set one up",
                if looked.is_empty() {
                    "no path, HOME is not set".to_string()
                } else {
                    looked.join(", ")
                }
            ))
        }
    }
}

pub fn sandbox(probe: &Probe<'_>) -> Outcome {
    let unsafe_on = probe
        .env
        .get(UNSAFE_ENV)
        .is_some_and(|v| ap_core::env::flag_value(v));
    match (&probe.sandbox, unsafe_on) {
        // The sandbox's own reason is not repeated: it names the same switch.
        (Err(_), true) => Outcome::WarnText(format!(
            "services run WITHOUT a sandbox: {UNSAFE_ENV} is set (the sandbox cannot start here)"
        )),
        (Ok(()), true) => Outcome::WarnText(format!(
            "services run WITHOUT a sandbox: {UNSAFE_ENV} is set"
        )),
        (Err(why), false) => Outcome::Missing(Cause::SandboxUnavailable { why: why.clone() }),
        (Ok(()), false) => {
            Outcome::Ok("bubblewrap, user namespaces, python3 and setpriv are in place".to_string())
        }
    }
}

/// `claude auth status` makes no model call. Only an explicit `loggedIn:
/// false` is Missing: a run that cannot tell (a timeout, a launch failure,
/// odd output) is a warning, as the agent's own start check lets it pass.
pub fn claude(probe: &Probe<'_>) -> Outcome {
    if !on_path_in("claude", &probe.path) {
        return Outcome::Missing(missing_tool("claude"));
    }
    match (probe.run)(&["claude", "auth", "status"], CLAUDE_TIMEOUT) {
        Ok(out) if status_says_logged_out(&out.stdout) => {
            Outcome::MissingText(NOT_LOGGED_IN_MESSAGE.to_string())
        }
        Ok(_) => Outcome::Ok("claude did not report a logout".to_string()),
        Err(e) => Outcome::WarnText(format!("claude auth status could not run: {e}")),
    }
}

/// The check `preflight` of a local build makes: `sudo -n -l osc build` asks
/// whether sudo would run `osc build` without a password, and runs nothing.
/// sudo may still log the refused attempt. Root needs no sudo, so no row.
pub fn sudo(probe: &Probe<'_>) -> Outcome {
    if probe.uid == 0 {
        return Outcome::NotApplicable;
    }
    match (probe.run)(&["sudo", "-n", "-l", "osc", "build"], SUDO_TIMEOUT) {
        Ok(out) if out.success => {
            Outcome::Ok("sudo allows `osc build` without a password".to_string())
        }
        Ok(_) => Outcome::MissingText(
            "`osc build` needs root and sudo would not run `osc build` without a password: \
             add a NOPASSWD sudoers rule for `osc build` for this user, or run auto-package \
             as root"
                .to_string(),
        ),
        Err(e) => Outcome::WarnText(format!("sudo cannot be checked: {e}")),
    }
}

/// The free space check of `--local`, as the build itself makes it.
pub fn build_root(probe: &Probe<'_>) -> Outcome {
    match (probe.build_root)() {
        Ok((free, dir)) => Outcome::Ok(format!(
            "{:.1} GiB free where {} is",
            free as f64 / (1u64 << 30) as f64,
            dir.display()
        )),
        Err(why) => Outcome::MissingText(why),
    }
}

pub fn cargo(probe: &Probe<'_>) -> Outcome {
    match (
        on_path_in("cargo", &probe.path),
        on_path_in("rustup", &probe.path),
    ) {
        (true, _) => Outcome::Ok("cargo is on PATH".to_string()),
        (false, true) => Outcome::WarnText(
            "rustup is on PATH but cargo is not; the sandbox uses /usr/bin/cargo or \
             /bin/cargo, not PATH"
                .to_string(),
        ),
        (false, false) => Outcome::Missing(missing_tool("cargo")),
    }
}

/// What the rustup rows are read from: the installed toolchains, the host
/// default, and the choice the sandbox makes from them (computed once).
struct Sandboxed {
    installed: Vec<String>,
    /// Warnings from listing the rustup home (a link that is not bound).
    listing: Vec<String>,
    default: Option<String>,
    choice: Choice,
}

enum Toolchains {
    /// `cargo` is not rustup's proxy: the sandbox never reads rustup.
    NotRustup,
    /// The rustup home exists but the sandbox refuses it.
    Refused(String),
    Read(Sandboxed),
}

/// Read the rustup home the way the sandbox does and choose a toolchain with
/// the sandbox's own `select_toolchain` (a source pin is not known here).
fn toolchains(probe: &Probe<'_>) -> Toolchains {
    if !probe.cargo_is_rustup {
        return Toolchains::NotRustup;
    }
    let (installed, listing, default) = match resolve_rustup_home(&probe.rustup_vars, probe.uid) {
        RustupHome::Refused(refused) => return Toolchains::Refused(refused.to_string()),
        RustupHome::Absent => (Vec::new(), Vec::new(), None),
        RustupHome::Usable(dir) => {
            let (installed, listing) = installed_toolchains(&dir);
            (installed, listing, host_default(&dir))
        }
    };
    let choice = select_toolchain(&Inputs {
        pin_file: None,
        installed: &installed,
        host_default: default.as_deref(),
        cargo_is_rustup: true,
    });
    Toolchains::Read(Sandboxed {
        installed,
        listing,
        default,
        choice,
    })
}

fn no_toolchain() -> Outcome {
    Outcome::Missing(Cause::ToolchainUnset {
        tool: "cargo".to_string(),
    })
}

/// What the sandbox will do, for a rustup row's text.
fn sandbox_will(choice: &Choice) -> String {
    match choice {
        Choice::Selected { name, why, .. } => {
            format!("the sandbox will use {name} ({why})")
        }
        Choice::NoToolchain => "no stable toolchain is installed: run \
             `rustup toolchain install stable`"
            .to_string(),
        Choice::PlainCargo => "cargo is not rustup".to_string(),
    }
}

/// The host's default toolchain of rustup. Hidden when `cargo` is not
/// rustup's proxy, and when the rustup home is refused (the sandbox row
/// names the refusal then). Missing only when nothing is installed.
pub fn rustup_default(probe: &Probe<'_>) -> Outcome {
    match toolchains(probe) {
        Toolchains::NotRustup | Toolchains::Refused(_) => Outcome::NotApplicable,
        Toolchains::Read(read) if read.installed.is_empty() => no_toolchain(),
        Toolchains::Read(read) => match &read.default {
            None => Outcome::WarnText(format!(
                "rustup has no default toolchain; {}",
                sandbox_will(&read.choice)
            )),
            Some(default) => match installed_match(default, &read.installed) {
                Some(name) => Outcome::Ok(format!("default toolchain {name}")),
                None => Outcome::WarnText(format!(
                    "rustup default toolchain {default} is not installed; {}",
                    sandbox_will(&read.choice)
                )),
            },
        },
    }
}

/// The toolchain the service sandbox would run cargo with.
pub fn sandbox_toolchain(probe: &Probe<'_>) -> Outcome {
    match toolchains(probe) {
        Toolchains::NotRustup => {
            Outcome::Ok("cargo is a regular binary, no rustup involved".to_string())
        }
        Toolchains::Refused(why) => Outcome::Missing(Cause::SandboxUnavailable { why }),
        Toolchains::Read(read) => match read.choice {
            Choice::PlainCargo => {
                Outcome::Ok("cargo is a regular binary, no rustup involved".to_string())
            }
            Choice::NoToolchain => no_toolchain(),
            Choice::Selected {
                name,
                why,
                warnings,
            } => {
                let text = format!("{name} ({why})");
                let all: Vec<String> = read.listing.into_iter().chain(warnings).collect();
                if all.is_empty() {
                    Outcome::Ok(text)
                } else {
                    Outcome::WarnText(format!("{text}: {}", all.join("; ")))
                }
            }
        },
    }
}

pub fn github_token(probe: &Probe<'_>) -> Outcome {
    if has_value(probe, "GITHUB_TOKEN") || has_value(probe, "GH_TOKEN") {
        Outcome::Ok("a GitHub token is set".to_string())
    } else if probe.gh_hosts.iter().any(|f| f.is_file()) {
        Outcome::Ok("gh has a login file".to_string())
    } else {
        Outcome::WarnText(
            "no GitHub token: unauthenticated GitHub requests are limited to 60 an hour per \
             IP; install the gh CLI and run `gh auth login`, or set GITHUB_TOKEN or GH_TOKEN"
                .to_string(),
        )
    }
}

pub fn gitlab_token(probe: &Probe<'_>) -> Outcome {
    if has_value(probe, "GITLAB_TOKEN") {
        Outcome::Ok("GITLAB_TOKEN is set".to_string())
    } else {
        Outcome::WarnText(
            "no GITLAB_TOKEN: gitlab.com rate-limits anonymous requests; set GITLAB_TOKEN \
             for authenticated access"
                .to_string(),
        )
    }
}

/// The OBS account: `osc whois` answers only with working credentials. The
/// keyring password is asked for here and nowhere else.
pub fn osc_whois(probe: &Probe<'_>) -> Outcome {
    match (probe.run)(&["osc", "whois"], WHOIS_TIMEOUT) {
        Ok(out) if out.success => {
            Outcome::Ok("osc whois answered for the configured account".to_string())
        }
        Ok(out) => Outcome::WarnText(format!(
            "osc whois failed: {}",
            out.stderr.lines().next().unwrap_or("no output")
        )),
        Err(e) => Outcome::WarnText(format!("osc whois could not run: {e}")),
    }
}

/// A name is a plain file name: not empty, not `.` or `..`, no `/`, and no
/// control character (a NUL or an escape sequence).
fn valid_service_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|c| c == '/' || c.is_control())
}

/// An OBS source service: its helper under the services directory, which must
/// be an executable file. The names of a `_service` are already limited to an
/// allow-list when parsed; a run's own list is checked here.
pub fn service(probe: &Probe<'_>, name: &str) -> Outcome {
    if !valid_service_name(name) {
        return Outcome::MissingText(format!("invalid service name {}", printable(name)));
    }
    let path = probe.services_dir.join(name);
    if is_executable(&path) {
        Outcome::Ok(format!("{} is installed", path.display()))
    } else {
        Outcome::Missing(Cause::MissingTool {
            tool: name.to_string(),
            path: Some(path),
        })
    }
}
