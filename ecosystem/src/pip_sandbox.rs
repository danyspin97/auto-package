//! The bubblewrap sandbox pip runs in.
//!
//! Resolving a package can build its sdist, and building runs the sdist's
//! `setup.py` or PEP 517 backend (and installs its `build-system.requires`),
//! all of it code from the index running as the user. The sandbox gives that
//! code:
//!
//! - the system read-only (`/usr`, `/lib*`, `/bin`, `/sbin`, and only the
//!   `/etc` entries pip, TLS, DNS and compilers need),
//! - an empty `tmpfs` home (`/home/sandbox`), `/tmp` and `/run`, which are the
//!   only writable places and vanish with the process; pip's cache lives
//!   there too, so nothing a backend writes survives to poison a later run,
//! - the network (resolution needs the index),
//! - its own PID, IPC, UTS, cgroup and user namespaces, a new session (no
//!   terminal injection) and `--die-with-parent`.
//!
//! It does not get the real home directory, `~/.ssh`, `~/.config`, XDG
//! directories, `/run/user` (D-Bus, ssh-agent and gpg-agent sockets), `/var`,
//! `/opt` or any environment variable beyond the pip allowlist.
//!
//! Three things are bound in on purpose, each read-only and each as a single
//! file or directory, never a parent:
//!
//! - the interpreter prefix, when pip or its Python live outside the system
//!   directories (a venv, `/opt/python`). Of that prefix only its `bin`,
//!   `lib`, `lib64`, `include` and `share/python*` directories and its
//!   `pyvenv.cfg` are bound, never the rest of it (`~/tools` next to
//!   `~/tools/bin` keeps its own files out of the sandbox). A prefix with a
//!   credential-like file at its top level, such as `.netrc` or `id_ed25519`,
//!   is refused;
//! - pip's own config (`PIP_CONFIG_FILE`, `~/.config/pip/pip.conf`,
//!   `~/.pip/pip.conf`), since private indexes are configured there. Index
//!   URLs in those files, and in `PIP_INDEX_URL` and `PIP_EXTRA_INDEX_URL`,
//!   can carry credentials, and a backend can read them. They are passed on
//!   as they are, because stripping them would break private indexes; use an
//!   index credential that is only good for reading packages;
//! - `~/.netrc`, only when `AUTO_PACKAGE_PIP_NETRC=1`, for the same reason.
//!   The default is off because a netrc usually holds credentials for hosts
//!   other than the index.
//!
//! Custom CA bundles named by `SSL_CERT_FILE`, `SSL_CERT_DIR`,
//! `REQUESTS_CA_BUNDLE` and `PIP_CERT` are bound at the same path when they
//! live outside the system directories.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// The home directory inside the sandbox.
pub const SANDBOX_HOME: &str = "/home/sandbox";

/// Where the user's `PIP_CONFIG_FILE` appears inside the sandbox.
const SANDBOX_PIP_CONFIG_FILE: &str = "/home/sandbox/.pip-config-file";

/// Set to `1` to give the sandbox `~/.netrc`. Off by default.
pub const NETRC_ENV: &str = "AUTO_PACKAGE_PIP_NETRC";

use ap_core::sandbox::ETC_ENTRIES;

/// Directories the sandbox already has from the system mounts.
const SYSTEM_DIRS: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64"];

/// Directories directly under the home directory that are never bound whole:
/// the prefix of a tool installed at `~/.local/bin/pip` is `~/.local`, which
/// also holds keyrings. Venvs deeper inside (`~/.local/share/pipx/venvs/x`)
/// are fine.
const SENSITIVE_HOME_DIRS: &[&str] = &[".local", ".config", ".cache"];

/// Credential directories nothing may be bound from, at any depth.
const CREDENTIAL_HOME_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".docker",
    ".password-store",
    ".mozilla",
    ".thunderbird",
];

/// The directories of a non-system interpreter prefix that are bound, at
/// their own path. `share/python*` is added by `prefix_mounts`.
const PREFIX_DIRS: &[&str] = &["bin", "lib", "lib64", "include"];

/// Names at the top level of an interpreter prefix that mark it as holding
/// credentials. Such a prefix is refused: a prefix is not bound whole, but its
/// interpreter directories are, and a credential beside them would be left
/// exposed to the build. Matched by `is_credential_entry`.
const CREDENTIAL_ENTRY_NAMES: &[&str] = &[
    ".netrc",
    ".pypirc",
    ".env",
    ".git-credentials",
    ".config",
    ".ssh",
    ".gnupg",
    ".aws",
];

/// Variables set explicitly inside the sandbox, so the host's values are not
/// copied.
const OWN_VARS: &[&str] = &[
    "HOME",
    "PATH",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "PIP_CONFIG_FILE",
];

/// Variables holding a path to a CA bundle file.
const CA_FILE_VARS: &[&str] = &["SSL_CERT_FILE", "REQUESTS_CA_BUNDLE", "PIP_CERT"];

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The program is not on `PATH`; callers treat this like a failed launch.
    #[error("{0} not found on PATH")]
    ProgramNotFound(String),

    #[error(
        "cannot sandbox {program}: it lives under {prefix}, which holds more than the \
         interpreter ({why}). Install pip in a virtualenv or use the system pip, or pass \
         --unsafe-resolve to resolve without the sandbox"
    )]
    UnsafePrefix {
        program: String,
        prefix: PathBuf,
        why: &'static str,
    },

    #[error("{var} points at {path}, which is {why}; not exposing it to the sandbox")]
    UnsafePath {
        var: String,
        path: PathBuf,
        why: &'static str,
    },
}

/// What the host contributes to one sandboxed pip run.
pub struct Host {
    /// The real home directory. Nothing in it is bound except the files named
    /// in the module documentation.
    pub home: PathBuf,
    /// The host's `PATH`, used to find the program.
    pub path: OsString,
    /// The allowlisted variables of the host (see `pip::pip_env`).
    pub vars: Vec<(OsString, OsString)>,
    /// Whether `~/.netrc` is shared with the sandbox.
    pub netrc: bool,
}

mod bwrap;
mod interpreter;
mod paths;

#[cfg(test)]
mod tests;

pub use bwrap::*;
use interpreter::*;
use paths::*;
