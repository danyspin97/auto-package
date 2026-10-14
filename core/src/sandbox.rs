//! Helpers shared by the bubblewrap sandboxes.
//!
//! Each caller builds its own argument list (what is mounted differs per
//! tool); this module holds the parts that are the same everywhere: the
//! read-only system mounts and the "does bubblewrap work here" probe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// `bwrap` arguments that mount the system read-only: `/usr` (required), and
/// `/bin`, `/lib`, `/lib64`, `/sbin` when present (symlinks into `/usr` on
/// merged-usr systems). `/etc` is left to the caller, since how much of it a
/// tool needs differs.
pub fn system_ro_binds() -> Vec<String> {
    let mut args = vec!["--ro-bind".to_string(), "/usr".into(), "/usr".into()];
    for dir in ["/bin", "/lib", "/lib64", "/sbin"] {
        args.extend(["--ro-bind-try".to_string(), dir.into(), dir.into()]);
    }
    args
}

/// The `bwrap` executable on `PATH`, as an absolute path.
///
/// Callers clear the child's environment, and a relative program name would
/// then be looked up in whatever `PATH` that leaves, so it is resolved once
/// here.
pub fn bwrap_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .filter(|d| d.is_absolute())
            .map(|d| d.join("bwrap"))
            .find(|p| p.is_file())
    })
    .as_deref()
}

/// Whether a sandbox that shares the network and mounts only the system can
/// run a program here. Probed once per process: `bwrap` being installed is
/// not enough, unprivileged user namespaces are often disabled.
pub fn bwrap_works() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| {
        let Some(bwrap) = bwrap_path() else {
            return false;
        };
        let mut args: Vec<String> = ["--unshare-all", "--share-net", "--die-with-parent"]
            .map(String::from)
            .into();
        args.extend(system_ro_binds());
        args.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"].map(String::from));
        args.extend(["--", "/usr/bin/true"].map(String::from));
        Command::new(bwrap)
            .env_clear()
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_is_mounted_read_only_and_usr_is_required() {
        let args = system_ro_binds();
        assert_eq!(args[..3], ["--ro-bind", "/usr", "/usr"]);
        for pair in args.chunks(3) {
            assert!(pair[0].starts_with("--ro-bind"), "{args:?}");
            assert_eq!(pair[1], pair[2]);
        }
        assert!(!args.iter().any(|a| a == "--bind"), "{args:?}");
    }
}
