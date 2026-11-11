//! Error classification of OBS output: the error type, the transient and not-found
//! tests of its text.

use super::*;
use crate::osc::checkout::*;
use crate::osc::query::*;
use ap_core::remedy::{Cause, Remedy, Retry};

/// The text of [`Error::ToolMissing`]: what is missing, and where it was looked
/// for when that is known. No install command.
fn missing_tool_text(tool: &str, path: Option<&Path>) -> String {
    match path {
        Some(path) => format!("{tool} is not installed ({} is missing)", path.display()),
        None => format!("{tool} is not installed (could not launch it)"),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run osc: {0}")]
    Launch(std::io::Error),

    #[error(transparent)]
    Process(#[from] proc::ProcessError),

    /// A launch failure or timeout, named after the osc subcommand it was for.
    #[error("{source} (during osc {cmd})")]
    Command {
        cmd: String,
        #[source]
        source: proc::ProcessError,
    },

    #[error("osc {cmd} failed (exit {code}):\n{}", osc_error_text(.stderr))]
    OscFailed {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("refusing to run the checkout's _service: {reason}")]
    ServiceRejected { reason: String },

    #[error("cannot sandbox the checkout's _service: {0}")]
    ServiceSandbox(String),

    /// A tool the machine needs is not installed, or could not be launched.
    /// `path` is where it was looked for, when that is known. The install
    /// command is not in the text: it is built from the remedy.
    #[error("{}", missing_tool_text(.tool, .path.as_deref()))]
    ToolMissing { tool: String, path: Option<PathBuf> },

    /// A Rust toolchain has no default to run with, or is not installed, so a
    /// service that runs `tool` cannot start.
    #[error("no Rust toolchain is available for {tool}")]
    ToolchainUnset { tool: String },

    /// The host's rustup home exists but is not safe to show a service, so
    /// cargo has no toolchains there. Nothing was run.
    #[error("rustup home {} was not used: {why}", .path.display())]
    RustupHomeRefused { path: PathBuf, why: String },

    #[error("nothing to publish in {0}: no spec or archive found")]
    NothingToPublish(PathBuf),

    #[error("rpmspec --parse {spec} failed (exit {code}):\n{stderr}")]
    RpmspecFailed {
        spec: String,
        code: i32,
        stderr: String,
    },

    #[error(
        "refusing to run rpmspec on {spec}: it contains `{marker}`, which runs code or reads \
         files while expanding, and no sandbox (bubblewrap) is available to contain it"
    )]
    UnsandboxedMacro { spec: String, marker: String },

    #[error("osc build failed for {spec} (exit {code}); log at {log}")]
    BuildFailed {
        spec: String,
        code: i32,
        log: PathBuf,
    },

    /// Build failed for a reason that no agent fix can address.
    /// The caller should stop immediately and report the message.
    #[error("not actionable: {0}")]
    NotActionable(String),
    /// OBS does not build the package for the build's repository and arch
    /// (excluded or disabled there): nothing is built, so the package is
    /// skipped, not failed. The text says why.
    #[error("{0}")]
    Excluded(String),

    /// One or more RPM dependencies could not be resolved. `names` holds the
    /// raw RPM package names (e.g. `python314-puccinialin`) the caller
    /// should queue for building and retry this one against; `detail` holds
    /// the verbatim `nothing provides ...`/`(got version ...)` lines behind
    /// them, when OBS gave any — the only way to tell a genuinely missing
    /// package apart from a version constraint this spec itself wrote that
    /// the repo's one available version can never satisfy (e.g.
    /// `setuptools < 70.0.0` when only 80.9.0 is shipped).
    #[error("unresolvable RPM dependencies: {}", .names.join(", "))]
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Whether the output of a child process (osc's Python traceback, a tool's
/// error line) says the disk is full. This is the one place the ENOSPC wording
/// is matched: a child's failure reaches us as text, so there is no errno to
/// read. Errors this module holds as an [`std::io::Error`] use
/// [`ap_core::fsx::is_disk_full`] instead.
pub fn child_output_says_disk_full(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("no space left on device") || t.contains("[errno 28]")
}

/// The Rust tools a service can name when the machine lacks them.
const RUST_TOOLS: &[&str] = &["cargo", "rustc", "rustfmt", "rustup"];

/// The error for a failed service run whose output says the machine lacks a
/// Rust tool or its toolchain, typed from that output; `None` for other output.
/// Each line is judged alone, so a phrase split over two lines does not count,
/// and only by rustup's or the shell's own wording:
/// - rustup's "could not choose a version of X" (X is cargo, rustc or rustfmt,
///   cargo when the line names none), and "no default is configured" on a line
///   that also says rustup: `ToolchainUnset`;
/// - rustup's "toolchain '<x>' is not installed": `ToolchainUnset{cargo}`, the
///   tool a service runs through the pinned toolchain;
/// - a shell's "<tool>: command not found" or "<tool>: not found", or an exec's
///   "execvp <tool>: No such file or directory", for a tool in `RUST_TOOLS`:
///   `ToolMissing`.
/// A line that only mentions rustup (a crate's log, say) gives `None`.
pub fn service_tool_error(text: &str) -> Option<Error> {
    text.lines().find_map(line_tool_error)
}

fn line_tool_error(line: &str) -> Option<Error> {
    let unset = |tool: &str| Error::ToolchainUnset {
        tool: tool.to_string(),
    };
    if let Some((_, rest)) = line.split_once("rustup could not choose a version of ") {
        let named = rest.split_whitespace().next().unwrap_or_default();
        let named = named.trim_end_matches(|c: char| !c.is_alphanumeric());
        let tool = ["cargo", "rustc", "rustfmt"]
            .into_iter()
            .find(|t| *t == named)
            .unwrap_or("cargo");
        return Some(unset(tool));
    }
    if line.contains("rustup") && line.contains("no default is configured") {
        return Some(unset("cargo"));
    }
    if line.trim_start().starts_with("error: toolchain '") && line.contains("' is not installed") {
        return Some(unset("cargo"));
    }
    RUST_TOOLS
        .iter()
        .find(|tool| missing_program(line, tool))
        .map(|tool| Error::ToolMissing {
            tool: tool.to_string(),
            path: None,
        })
}

/// Whether `line` is a shell's or an exec's report that `tool` was not found.
fn missing_program(line: &str, tool: &str) -> bool {
    [
        format!(": {tool}: command not found"),
        format!(": {tool}: not found"),
        format!("execvp {tool}: No such file or directory"),
        format!(": {tool}: No such file or directory"),
    ]
    .iter()
    .any(|needle| line.contains(needle.as_str()))
}

/// Text that marks a failure of the machine or the network rather than of the
/// package: a dropped or refused connection, a name that did not resolve, a
/// timeout or an OBS 5xx. Matched case-insensitively. A full disk is typed
/// instead (see [`Error::is_disk_full`]).
pub fn text_is_transient(text: &str) -> bool {
    let t = text.to_lowercase();
    const MARKS: &[&str] = &[
        "connection refused",
        "connection reset",
        "connection aborted",
        "connection closed",
        "connection timed out",
        "timed out",
        "temporary failure in name resolution",
        "name or service not known",
        "could not resolve host",
        "failed to lookup address",
        "dns error",
        "network is unreachable",
        "no route to host",
        "bad gateway",
        "service unavailable",
        "gateway time-out",
        "gateway timeout",
        "rate limited (429)",
        "broken pipe",
        "unexpected eof",
        "tls handshake",
    ];
    if MARKS.iter().any(|m| t.contains(m)) {
        return true;
    }
    match http_status(text) {
        // An explicit status is the answer: a 404 for a project called
        // `timeout` is not a timeout, and a 504 is one whatever it says. A 429
        // is OBS asking us to slow down, which passes.
        Some(code) => code == 408 || code == 429 || code >= 500,
        // Without a status, a timeout word counts unless the text says plainly
        // that the project or package is not there.
        None => {
            !NOT_FOUND_PHRASES.iter().any(|p| t.contains(p)) && has_timeout_word(error_body(text))
        }
    }
}

/// Phrases that say plainly that a project or package does not exist.
const NOT_FOUND_PHRASES: &[&str] = &[
    "unknown project",
    "project not found",
    "no such project",
    "not found",
];

/// The part of an `osc ... failed (exit N):` message that the server or the
/// tool wrote, without the command line that names the project or package.
/// Text without that header is returned whole.
fn error_body(text: &str) -> &str {
    match text.find(" failed (exit ") {
        Some(at) => text[at..].split_once(':').map_or("", |(_, rest)| rest),
        None => text,
    }
}

/// Whether `timeout` is a word of its own in `text`: `Error: Timeout` and
/// `connection timeout:` have it. A quoted word (`'timeout'`, a project's name)
/// does not, nor does a name with a separator (`home:alice:timeout`,
/// `python-timeout-decorator`, `pkg.timeout`) or `--timeouts`.
fn has_timeout_word(text: &str) -> bool {
    text.split_whitespace()
        .filter(|tok| !tok.contains(['\'', '"']))
        .any(|tok| {
            let word = tok
                .trim_start_matches(['(', '['])
                .trim_end_matches([':', ',', '.', ';', ')', ']', '!', '?']);
            !word.contains([':', '-', '.', '/']) && word.eq_ignore_ascii_case("timeout")
        })
}

/// Whether a failed project lookup answers that the project does not carry the
/// package: a 404 (no such project), or a 403 when `path_project` is set (a
/// project we only read from may refuse us). Anything else, a 401 or
/// "unauthorized" included, a launch failure, a timeout or an unrecognised
/// error, is a failure to look, not an answer.
pub fn lookup_says_absent(text: &str, path_project: bool) -> bool {
    if text_is_transient(text) {
        return false;
    }
    match http_status(text) {
        Some(404) => true,
        Some(403) => path_project,
        Some(_) => false,
        None => {
            let t = text.to_lowercase();
            t.contains("unknown project")
                || t.contains("project not found")
                || t.contains("no such project")
        }
    }
}

/// Marker in the message of a build whose RPMs could not be fetched after OBS
/// reported it built.
pub(super) const DOWNLOAD_FAILED: &str = "succeeded but downloading the built RPMs failed";

impl Error {
    /// Whether OBS answered 404 to the request: the package or project asked
    /// for is not there.
    pub fn is_not_found(&self) -> bool {
        match self {
            Error::OscFailed { stderr, .. } => super::names::is_not_found(stderr),
            _ => false,
        }
    }

    /// Whether the failure is a full disk: the OS reported `ENOSPC`, or the
    /// output of the osc child says so (its Python traceback, a tool's error
    /// line). The OS case is typed; the child case has only its text.
    pub fn is_disk_full(&self) -> bool {
        match self {
            Error::Launch(e) | Error::Io(e) => ap_core::fsx::is_disk_full(e),
            Error::Process(p) | Error::Command { source: p, .. } => match p {
                proc::ProcessError::Io(e) | proc::ProcessError::Launch { source: e, .. } => {
                    ap_core::fsx::is_disk_full(e)
                }
                proc::ProcessError::Timeout { .. } | proc::ProcessError::OutputLost { .. } => false,
            },
            Error::OscFailed { stderr, .. } => child_output_says_disk_full(stderr),
            _ => false,
        }
    }

    /// What must be done on this machine before a rerun can succeed, when the
    /// failure is a missing tool or an unset toolchain.
    pub fn remedy(&self) -> Option<Remedy> {
        let cause = match self {
            Error::ToolMissing { tool, path } => Cause::MissingTool {
                tool: tool.clone(),
                path: path.clone(),
            },
            Error::ToolchainUnset { tool } => Cause::ToolchainUnset { tool: tool.clone() },
            Error::RustupHomeRefused { path, why } => Cause::SandboxUnavailable {
                why: format!("rustup home {} was not used: {why}", path.display()),
            },
            _ => return None,
        };
        Some(Remedy {
            cause,
            retry: Retry::SameCommand,
        })
    }

    /// Whether the failure says nothing about the package and a later attempt
    /// may succeed: the network, OBS or this machine failed. A wait that gave
    /// up polling counts too (the build itself may have been fine).
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Launch(_) | Error::Process(_) | Error::Io(_) | Error::Command { .. } => {
                match self {
                    Error::Io(e) | Error::Launch(e) => io_is_transient(e),
                    Error::Process(p) | Error::Command { source: p, .. } => match p {
                        proc::ProcessError::Timeout { .. }
                        | proc::ProcessError::OutputLost { .. } => true,
                        proc::ProcessError::Io(e)
                        | proc::ProcessError::Launch { source: e, .. } => io_is_transient(e),
                    },
                    _ => false,
                }
            }
            Error::OscFailed { code, stderr, .. } => {
                // Our own wait gave up (`-1`) for another reason than a shutdown.
                *code == 28
                    || (*code == -1 && stderr != "interrupted")
                    || text_is_transient(stderr)
                    || child_output_says_disk_full(stderr)
            }
            Error::NotActionable(m) => m.contains(DOWNLOAD_FAILED),
            _ => false,
        }
    }
}

fn io_is_transient(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        e.kind(),
        K::TimedOut
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::ConnectionRefused
            | K::BrokenPipe
            | K::NetworkUnreachable
            | K::HostUnreachable
    ) || ap_core::fsx::is_disk_full(e)
        || text_is_transient(&e.to_string())
}
