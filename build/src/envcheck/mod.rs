//! The environment auto-package needs, checked in one place.
//!
//! [`CHECKS`] is the table. Each row says what it checks, whether it is
//! required, when it applies to a run ([`Needs`]) and how it is detected
//! ([`Probe`]). [`run`] applies the table to one run's needs and a probe, and
//! [`host_probe`] reads the real machine. The probe carries every fact a check
//! reads, so tests inject them. A check starts a process only through
//! [`Probe::run`], and only the commands in [`ALLOWED_COMMANDS`] are ever run.

#[cfg(test)]
mod checks_tests;
mod probes;
#[cfg(test)]
mod rustup_tests;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ap_core::remedy::{Cause, HostDistro, install_hint, parse_os_release, printable, suse_package};

pub use probes::{on_path_in, tool_on_path};

use crate::osc::Error;
use crate::osc::check_build_root_space;
use crate::osc::local_build;
use crate::proc::{self, Emit};
use crate::service_sandbox::{
    CARGO_PATHS, HostVars, SERVICE_DIR, UNSAFE_ENV, cargo_is_rustup, oscrc_paths, sandbox_ready,
};

/// Whether a missing check stops a run that needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Required {
    Yes,
    No,
}

/// What one run needs from the machine.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Needs {
    /// The claude agent runs (a model is asked for a spec repair).
    pub agent: bool,
    /// Packages are built locally with `osc build` (`--local`).
    pub local: bool,
    /// OBS source services the run calls, by name (`cargo_vendor`).
    pub services: Vec<String>,
    /// A Rust source is built or vendored, so the rustup rows apply.
    pub rust: bool,
    /// The run may reach the network and the OBS account.
    pub online: bool,
}

/// The result of a command the checks ran: its exit status and output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The facts a check reads. [`host_probe`] fills it from the machine; tests
/// fill it with temporary files and a scripted `run`.
pub struct Probe<'a> {
    /// The `PATH` that tools are looked for on.
    pub path: OsString,
    /// Where OBS service helpers are installed (`/usr/lib/obs/service`).
    pub services_dir: PathBuf,
    /// The text of `/etc/os-release` (empty when there is none).
    pub os_release: String,
    /// The oscrc files osc reads, in order.
    pub oscrc_paths: Vec<PathBuf>,
    /// The `gh` login files (`hosts.yml`), in order.
    pub gh_hosts: Vec<PathBuf>,
    /// The variables the rustup home is derived from.
    pub rustup_vars: HostVars,
    /// The effective user id, for the ownership checks on the rustup home.
    pub uid: u32,
    /// Whether `cargo` resolves to rustup (the sandbox's own check).
    pub cargo_is_rustup: bool,
    /// Whether the service sandbox can start, or why not.
    pub sandbox: Result<(), String>,
    /// The free space of the build root (its bytes and directory, or why it
    /// is not enough). A call runs `df`, so only the local rows call it.
    pub build_root: Box<dyn Fn() -> Result<(u64, PathBuf), String>>,
    /// The environment variables the checks read (tokens, the unsafe switch).
    pub env: BTreeMap<String, String>,
    /// Runs one of [`ALLOWED_COMMANDS`] with a time bound and returns its
    /// output. `Err` when it cannot run or does not finish in time.
    pub run: &'a dyn Fn(&[&str], Duration) -> Result<Output, String>,
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Present; the text says what was found.
    Ok(String),
    /// Required and absent; the cause names the fix.
    Missing(Cause),
    /// Optional and absent; the cause names the fix.
    Warn(Cause),
    /// Required and absent, for a fix that is not a [`Cause`]; the text says it.
    MissingText(String),
    /// Optional and not right, for a fix that is not a [`Cause`].
    WarnText(String),
    /// Does not apply to this machine's configuration: the row is left out.
    NotApplicable,
}

/// One row of [`CHECKS`].
pub struct Check {
    pub id: &'static str,
    pub name: &'static str,
    pub required: Required,
    pub applies: fn(&Needs) -> bool,
    pub detect: fn(&Probe<'_>) -> Outcome,
}

/// One checked row of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// The check's id; `service:<name>` for an OBS service.
    pub id: String,
    pub name: String,
    pub required: Required,
    pub outcome: Outcome,
    /// How to fix a `Missing` or `Warn` cause on this host.
    pub hint: Option<String>,
    /// The openSUSE package of a `Missing` or `Warn` cause, when known.
    pub package: Option<&'static str>,
}

/// The commands a check may run. Nothing else is started: no `claude -p`, no
/// network command beyond `osc whois`, which only an online run asks for.
pub const ALLOWED_COMMANDS: &[&[&str]] = &[
    &["claude", "auth", "status"],
    &["sudo", "-n", "-l", "osc", "build"],
    &["osc", "whois"],
];

fn always(_: &Needs) -> bool {
    true
}

fn agent(needs: &Needs) -> bool {
    needs.agent
}

fn local(needs: &Needs) -> bool {
    needs.local
}

fn rust(needs: &Needs) -> bool {
    needs.rust
}

fn online(needs: &Needs) -> bool {
    needs.online
}

/// Every check, in the order a report shows them.
pub const CHECKS: &[Check] = &[
    Check {
        id: "osc",
        name: "osc on PATH",
        required: Required::Yes,
        applies: always,
        detect: probes::osc,
    },
    Check {
        id: "oscrc",
        name: "an oscrc file",
        required: Required::Yes,
        applies: always,
        detect: probes::oscrc,
    },
    Check {
        id: "rpmspec",
        name: "rpmspec on PATH",
        required: Required::Yes,
        applies: always,
        detect: probes::rpmspec,
    },
    Check {
        id: "quilt",
        name: "quilt on PATH",
        required: Required::Yes,
        applies: always,
        detect: probes::quilt,
    },
    Check {
        id: "sandbox",
        name: "service sandbox (bubblewrap)",
        required: Required::Yes,
        applies: always,
        detect: probes::sandbox,
    },
    Check {
        id: "claude",
        name: "claude logged in",
        required: Required::Yes,
        applies: agent,
        detect: probes::claude,
    },
    Check {
        id: "build",
        name: "osc build script on PATH",
        required: Required::Yes,
        applies: local,
        detect: probes::build_script,
    },
    Check {
        id: "sudo",
        name: "passwordless sudo for osc build",
        required: Required::Yes,
        applies: local,
        detect: probes::sudo,
    },
    Check {
        id: "build-root",
        name: "free space for the build root",
        required: Required::Yes,
        applies: local,
        detect: probes::build_root,
    },
    Check {
        id: "cc",
        name: "a C compiler (gcc or cc)",
        required: Required::No,
        applies: local,
        detect: probes::cc,
    },
    Check {
        id: "cargo",
        name: "cargo on PATH",
        required: Required::Yes,
        applies: rust,
        detect: probes::cargo,
    },
    Check {
        id: "rustup-default",
        name: "rustup default toolchain",
        required: Required::Yes,
        applies: rust,
        detect: probes::rustup_default,
    },
    Check {
        id: "sandbox-toolchain",
        name: "rust toolchain for the service sandbox",
        required: Required::Yes,
        applies: rust,
        detect: probes::sandbox_toolchain,
    },
    Check {
        id: "github-token",
        name: "GitHub token",
        required: Required::No,
        applies: always,
        detect: probes::github_token,
    },
    Check {
        id: "gitlab-token",
        name: "GitLab token",
        required: Required::No,
        applies: always,
        detect: probes::gitlab_token,
    },
    Check {
        id: "gpg",
        name: "gpg on PATH",
        required: Required::No,
        applies: always,
        detect: probes::gpg,
    },
    Check {
        id: "osc-whois",
        name: "osc reaches OBS with your credentials",
        required: Required::No,
        applies: online,
        detect: probes::osc_whois,
    },
];

/// Apply the table to one run. Rows that do not apply, and rows whose
/// detection says [`Outcome::NotApplicable`], are left out. Each OBS service
/// the run names gets its own required row, after the table's rows.
pub fn run(needs: &Needs, probe: &Probe<'_>) -> Vec<CheckResult> {
    let host = parse_os_release(&probe.os_release);
    let mut rows: Vec<CheckResult> = CHECKS
        .iter()
        .filter(|check| (check.applies)(needs))
        .filter_map(|check| {
            let outcome = (check.detect)(probe);
            (outcome != Outcome::NotApplicable).then(|| {
                result(
                    check.id.to_string(),
                    check.name.to_string(),
                    check.required,
                    outcome,
                    host,
                )
            })
        })
        .collect();
    let services: BTreeSet<&str> = needs.services.iter().map(String::as_str).collect();
    rows.extend(services.into_iter().map(|name| {
        result(
            format!("service:{}", printable(name)),
            format!("OBS service {}", printable(name)),
            Required::Yes,
            probes::service(probe, name),
            host,
        )
    }));
    rows
}

fn result(
    id: String,
    name: String,
    required: Required,
    outcome: Outcome,
    host: HostDistro,
) -> CheckResult {
    let (hint, package) = match &outcome {
        Outcome::Missing(cause) | Outcome::Warn(cause) => {
            (install_hint(cause, &host), suse_package(cause, &host))
        }
        _ => (None, None),
    };
    CheckResult {
        id,
        name,
        required,
        outcome,
        hint,
        package,
    }
}

/// One `sudo zypper install` line for every openSUSE package that a `Missing`
/// row needs, each named once, in sorted order. `None` when no such row
/// names a package (on a host that is not openSUSE, none does).
pub fn zypper_line(results: &[CheckResult]) -> Option<String> {
    let packages: BTreeSet<&str> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Missing(_)))
        .filter_map(|r| r.package)
        .collect();
    (!packages.is_empty()).then(|| {
        let names: Vec<&str> = packages.into_iter().collect();
        format!("sudo zypper install {}", names.join(" "))
    })
}

/// The probe of this machine. It runs the sandbox probe and reads the rustup
/// home now, so call it when the run starts. The build root's free space is
/// read only when a local row applies, and `local_build()` is read then too,
/// so `enable_local_build` may run after this.
pub fn host_probe() -> Probe<'static> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let osc_config_set = std::env::var_os("OSC_CONFIG").is_some_and(|v| !v.is_empty());
    let env: BTreeMap<String, String> = ["GITHUB_TOKEN", "GH_TOKEN", "GITLAB_TOKEN", UNSAFE_ENV]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    Probe {
        path: std::env::var_os("PATH").unwrap_or_default(),
        services_dir: PathBuf::from(SERVICE_DIR),
        os_release: std::fs::read_to_string("/etc/os-release").unwrap_or_default(),
        oscrc_paths: oscrc_candidates(oscrc_paths(), osc_config_set),
        gh_hosts: gh_hosts(),
        rustup_vars: HostVars::from_env(),
        uid,
        cargo_is_rustup: cargo_is_rustup(&CARGO_PATHS),
        sandbox: sandbox_ready().map(|_| ()).map_err(|e| match e {
            Error::ServiceSandbox(why) => why,
            other => other.to_string(),
        }),
        // Read at row time, not here: `enable_local_build` may run after this.
        build_root: Box::new(|| {
            check_build_root_space(&local_build().cloned().unwrap_or_default())
                .map_err(|e| e.to_string())
        }),
        env,
        run: &host_run,
    }
}

/// The oscrc files osc reads. With `OSC_CONFIG` set, osc reads that file alone
/// (`oscrc_paths` lists it first), so it is the only candidate.
fn oscrc_candidates(mut paths: Vec<PathBuf>, osc_config_set: bool) -> Vec<PathBuf> {
    if osc_config_set {
        paths.truncate(1);
    }
    paths
}

/// The `hosts.yml` files of `gh`, where its config directory is set.
fn gh_hosts() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("GH_CONFIG_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.push(Path::new(&xdg).join("gh"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".config").join("gh"));
    }
    dirs.into_iter().map(|d| d.join("hosts.yml")).collect()
}

/// Run an allowed command in a cleared environment: only the variables that
/// the tools need to find their configuration and a session.
fn host_run(argv: &[&str], timeout: Duration) -> Result<Output, String> {
    if !ALLOWED_COMMANDS.iter().any(|allowed| *allowed == argv) {
        return Err(format!(
            "{} is not a command the checks run",
            argv.join(" ")
        ));
    }
    let Some((program, args)) = argv.split_first() else {
        return Err("no command".to_string());
    };
    let env = command_env(program);
    let out = proc::capture_with_timeout_env(
        program,
        args,
        Path::new("."),
        Emit::quiet(),
        timeout,
        Some(&env),
    )
    .map_err(|e| e.to_string())?;
    Ok(Output {
        success: out.success(),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

/// The environment a command runs in. `claude` gets the agent's environment,
/// the one `ensure_logged_in` checks with, so API keys, the settings `env`
/// block and a custom config dir count the same way. Other commands get a
/// short list: what they need to find their configuration and a session.
fn command_env(program: &str) -> BTreeMap<String, String> {
    if program == "claude" {
        return crate::agent::agent_env();
    }
    [
        "PATH",
        "HOME",
        "LANG",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "OSC_CONFIG",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect()
}
