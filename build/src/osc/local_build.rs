//! `osc build` on this machine: the build step of `--local`, in place of the
//! commit to OBS and the wait for its remote build.
//!
//! The build runs against the checkout as it is. Its output goes to the same
//! build log file the OBS log goes to, and its failure is classified by the
//! same log reader, so the fix loop sees no difference.

use super::*;
use crate::osc::build_retry::usable_build_log;
use crate::osc::failure::classify_build_failure;

/// The default floor of free space on the build root's filesystem.
pub const DEFAULT_MIN_FREE_GIB: u64 = 10;

/// Where `osc build` keeps its build root when none is given.
pub const DEFAULT_BUILD_ROOT: &str = "/var/tmp/build-root";

/// How long one local build may run.
const BUILD_TIMEOUT: Duration = Duration::from_secs(4 * 3600);

/// How long a build that ran past [`BUILD_TIMEOUT`] gets to stop after SIGTERM.
const STOP_GRACE: Duration = Duration::from_secs(60);

/// The directory under `rpms/` that holds the RPMs of local builds. OBS's own
/// RPMs stay in `rpms/`, so a local build's RPMs are never taken for published
/// ones, and `--prefer-pkgs` reads only these.
pub const LOCAL_RPMS_DIR: &str = "local";

/// How many lines at the end of a failed build's log are read for an
/// environment problem (sudo, a full disk).
const ENVIRONMENT_TAIL_LINES: usize = 60;

/// How a local build is set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBuild {
    /// The build root (`osc build --root`); `None` leaves osc its default.
    pub root: Option<PathBuf>,
    /// Refuse to start with less than this many GiB free where the root is.
    pub min_free_gib: u64,
}

impl Default for LocalBuild {
    fn default() -> Self {
        Self {
            root: None,
            min_free_gib: DEFAULT_MIN_FREE_GIB,
        }
    }
}

static SETTINGS: OnceLock<LocalBuild> = OnceLock::new();

/// Turn local builds on for this process. The first call wins.
pub fn enable_local_build(settings: LocalBuild) {
    let _ = SETTINGS.set(settings);
}

/// The local build settings, when `--local` was given.
pub fn local_build() -> Option<&'static LocalBuild> {
    SETTINGS.get()
}

/// The oscrc to hand to a build run as root (sudo changes `HOME`, so osc
/// would not find the user's credentials): `$XDG_CONFIG_HOME/osc/oscrc`,
/// `~/.config/osc/oscrc` or `~/.oscrc`, the first that exists.
pub(super) fn find_oscrc(
    home: Option<&Path>,
    xdg: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = xdg {
        candidates.push(xdg.join("osc").join("oscrc"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".config").join("osc").join("oscrc"));
        candidates.push(home.join(".oscrc"));
    }
    candidates.into_iter().find(|p| exists(p))
}

/// The command line of the build: `[sudo -n] osc [-c oscrc] build ...`, program
/// first. `sudo -n` never asks for a password, so a machine without
/// passwordless sudo fails at once instead of hanging on a prompt.
///
/// `local_pkgs` is the directory of this machine's local RPMs: the build keeps
/// its RPMs there, and dependencies are resolved from there first.
pub(super) fn build_argv(
    settings: &LocalBuild,
    repo: &str,
    arch: &str,
    spec: &str,
    local_pkgs: &Path,
    oscrc: Option<&Path>,
    as_root: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if !as_root {
        argv.extend(["sudo".to_string(), "-n".to_string()]);
    }
    argv.push("osc".to_string());
    if let Some(rc) = oscrc {
        argv.push("-c".to_string());
        argv.push(rc.display().to_string());
    }
    argv.extend(["build".to_string(), "--trust-all-projects".to_string()]);
    if let Some(root) = &settings.root {
        argv.push(format!("--root={}", root.display()));
    }
    argv.push(format!("--prefer-pkgs={}", local_pkgs.display()));
    argv.push(format!("--keep-pkgs={}", local_pkgs.display()));
    argv.extend([repo.to_string(), arch.to_string(), spec.to_string()]);
    argv
}

/// Free bytes from the output of `df -Pk <path>` (POSIX format: the fourth
/// field of the last line is the available 1 KiB blocks).
pub(super) fn parse_df_available(output: &str) -> Option<u64> {
    let line = output.lines().rfind(|l| !l.trim().is_empty())?;
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// The refusal for too little room, `None` when there is enough.
pub(super) fn free_space_refusal(available: u64, min_free_gib: u64, root: &Path) -> Option<String> {
    let floor = min_free_gib.saturating_mul(1 << 30);
    if available >= floor {
        return None;
    }
    Some(format!(
        "not enough free space for a local build: {:.1} GiB free where {} is, the floor is {} GiB. \
         Free space, point --local-root at a bigger filesystem, or lower --local-min-free-gib",
        available as f64 / (1u64 << 30) as f64,
        root.display(),
        min_free_gib
    ))
}

/// The closest existing directory at or above `path`: where a build root that
/// osc has yet to create will live.
pub(super) fn nearest_existing(path: &Path, is_dir: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    path.ancestors().find(|p| is_dir(p)).map(Path::to_path_buf)
}

/// The last `ENVIRONMENT_TAIL_LINES` lines of a log, with an OBS-style
/// `[  12s] ` prefix removed from each.
fn log_tail(log: &str) -> Vec<&str> {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(ENVIRONMENT_TAIL_LINES)..]
        .iter()
        .map(|l| {
            let l = l.trim_start();
            match l.strip_prefix('[') {
                Some(rest) => rest.split_once("] ").map_or(l, |(_, after)| after),
                None => l,
            }
            .trim_start()
        })
        .collect()
}

/// What a failed local build's log says about the machine rather than the
/// package: nothing an agent edit to the spec can fix. Only the end of the log
/// is read, and only lines that osc or sudo themselves print (sudo's own line,
/// an `error:` line), so a package's test output cannot pass for one.
pub(super) fn environment_problem(log: &str) -> Option<String> {
    let tail = log_tail(log);
    let sudo_refused = tail.iter().any(|l| {
        l.starts_with("sudo:")
            && (l.contains("a password is required") || l.contains("no tty present"))
    });
    if sudo_refused {
        return Some(
            "sudo asked for a password: allow this user to run `osc build` through sudo \
             without one (a NOPASSWD sudoers rule), or run auto-package as root"
                .to_string(),
        );
    }
    let disk_full = tail.iter().any(|l| {
        l.contains("No space left on device")
            && (l.starts_with("error:") || l.starts_with("osc") || l.starts_with("OSError"))
    });
    if disk_full {
        return Some(
            "the build root ran out of disk space: free space or use --local-root on a bigger \
             filesystem"
                .to_string(),
        );
    }
    None
}

/// Whether a log holds rpmbuild's own account of a package failing: the
/// failed phase, the RPM build errors, or the dependencies it could not
/// install for the build. Without one, osc failed for another reason.
pub(super) fn has_package_failure(log: &str) -> bool {
    log.contains("Bad exit status from")
        || log.contains("RPM build errors")
        || log.contains("Failed build dependencies")
}

/// The result of an `osc build` run from its exit code and its log: success,
/// a problem of this machine or of osc (not actionable by the agent), or a
/// failure of the package classified like an OBS log.
pub(super) fn local_outcome(code: i32, log_path: &Path, spec: &str) -> Result<(), Error> {
    if code == 0 {
        return Ok(());
    }
    let log = usable_build_log(log_path)?;
    if let Some(problem) = environment_problem(&log) {
        return Err(Error::NotActionable(problem));
    }
    match classify_build_failure(&log) {
        FailureKind::NoRepositories => Err(Error::NotActionable(
            "the project has no build repositories for this distro: configure them in OBS first"
                .to_string(),
        )),
        FailureKind::NotWorkingCopy => Err(Error::NotActionable(
            "osc checkout is not server-backed".to_string(),
        )),
        FailureKind::UnresolvableDeps { names, detail } => {
            Err(Error::UnresolvableDeps { names, detail })
        }
        FailureKind::BuildFailed if has_package_failure(&log) => Err(Error::BuildFailed {
            spec: spec.to_string(),
            code,
            log: log_path.to_owned(),
        }),
        FailureKind::BuildFailed => Err(Error::NotActionable(format!(
            "`osc build` failed (exit {code}) without a failure of the package in its log: \
             osc, the network, the repository or the build root is the problem; see {}",
            log_path.display()
        ))),
    }
}

/// How many `.rpm` files under `dir` were written at or after `since`.
pub(super) fn fresh_rpm_count(dir: &Path, since: SystemTime) -> usize {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == "rpm"))
        .filter(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|t| t >= since)
        })
        .count()
}

/// The one `*.spec` in `checkout`, or `<pkg>.spec` when there is none or several.
fn checkout_spec_name(checkout: &Path, pkg: &str) -> String {
    let files: Vec<String> = fs::read_dir(checkout)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    spec_file_name(&files, pkg)
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

fn environment_error(msg: impl Into<String>) -> Error {
    Error::NotActionable(msg.into())
}

/// Check that the build root's filesystem has the free space the settings ask
/// for. Returns the free bytes and the directory they were read from.
pub fn check_build_root_space(settings: &LocalBuild) -> Result<(u64, PathBuf), Error> {
    let here = Path::new(".");
    let root = settings
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BUILD_ROOT));
    let Some(on_disk) = nearest_existing(&root, |p| p.is_dir()) else {
        return Err(environment_error(format!(
            "no build root: no part of {} exists; give --local-root a path on a mounted filesystem",
            root.display()
        )));
    };
    let df = proc::capture_with_timeout(
        "df",
        &["-Pk".as_ref(), on_disk.as_os_str()],
        here,
        Emit::quiet(),
        Duration::from_secs(60),
    )
    .map_err(|e| {
        environment_error(format!(
            "cannot read the free space of {}: {e}",
            on_disk.display()
        ))
    })?;
    let Some(available) = parse_df_available(&df.stdout).filter(|_| df.success()) else {
        return Err(environment_error(format!(
            "cannot read the free space of {}: unexpected `df` output",
            on_disk.display()
        )));
    };
    if let Some(refusal) = free_space_refusal(available, settings.min_free_gib, &root) {
        return Err(environment_error(refusal));
    }
    Ok((available, on_disk))
}

/// Check, before the long build, that this machine can run it: osc is there,
/// root is available without a password, and the build root has room.
fn preflight(settings: &LocalBuild, as_root: bool, emit: Emit<'_>) -> Result<(), Error> {
    let here = Path::new(".");
    let (available, on_disk) = check_build_root_space(settings)?;
    match proc::capture_with_timeout(
        "osc",
        &["--version"],
        here,
        Emit::quiet(),
        Duration::from_secs(60),
    ) {
        Ok(_) => {}
        Err(proc::ProcessError::Launch { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Err(environment_error(
                "osc was not found: install the `osc` and `build` packages to build locally",
            ));
        }
        Err(e) => return Err(environment_error(format!("osc cannot be run: {e}"))),
    }
    if !as_root {
        // `sudo -l` asks whether sudo would run this command, with no password
        // (`-n`), so a sudoers rule that allows only `osc build` passes. The
        // command is not run.
        match proc::capture_with_timeout(
            "sudo",
            &["-n", "-l", "osc", "build"],
            here,
            Emit::quiet(),
            Duration::from_secs(60),
        ) {
            Ok(c) if c.success() => {}
            Ok(_) => {
                return Err(environment_error(
                    "`osc build` needs root and sudo would not run `osc build` without a password: \
                     add a NOPASSWD sudoers rule for `osc build` for this user, or run auto-package as root",
                ));
            }
            Err(proc::ProcessError::Launch { .. }) => {
                return Err(environment_error(
                    "sudo was not found: `osc build` needs root, so install sudo or run \
                     auto-package as root",
                ));
            }
            Err(e) => return Err(environment_error(format!("sudo cannot be run: {e}"))),
        }
    }
    emit.reporter.report(&Event::Progress {
        step: emit.step,
        message: format!(
            "local build: {:.1} GiB free at {}",
            available as f64 / (1u64 << 30) as f64,
            on_disk.display()
        ),
    });
    Ok(())
}

/// Build the package in `checkout` with `osc build` on this machine.
///
/// The output of the build is written to `log_path` as it runs, and its RPMs
/// are kept in `rpms_dir/local` (see [`LOCAL_RPMS_DIR`]). An exit code of 0
/// with no new RPM is an error. A failure of the package is classified from the
/// log exactly as an OBS build's is; a failure of osc or of this machine is
/// not actionable.
#[allow(clippy::too_many_arguments)]
pub fn local_build_package(
    settings: &LocalBuild,
    checkout: &Path,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    // One build root, one build at a time: `--jobs` may run several packages,
    // but their local builds queue here.
    static ONE_BUILD: Mutex<()> = Mutex::new(());
    let _one = ONE_BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let as_root = is_root();
    preflight(settings, as_root, emit)?;
    let local_pkgs = rpms_dir.join(LOCAL_RPMS_DIR);
    fs::create_dir_all(&local_pkgs)?;
    let spec = checkout_spec_name(checkout, pkg_name);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let oscrc = find_oscrc(home.as_deref(), xdg.as_deref(), |p| p.is_file());
    let argv = build_argv(
        settings,
        repo,
        arch,
        &spec,
        &local_pkgs,
        oscrc.as_deref(),
        as_root,
    );
    let started = SystemTime::now();
    let code = proc::run_logged(
        &argv[0],
        &argv[1..],
        checkout,
        log_path,
        emit,
        BUILD_TIMEOUT,
        STOP_GRACE,
    )
    .map_err(|source| Error::Command {
        cmd: "build".to_string(),
        source,
    })?;
    // The log holds what the build printed before it was stopped.
    let Some(code) = code else {
        return Err(environment_error(format!(
            "`osc build` of {pkg_name} ran past {} hours and was stopped; its output is in {}. \
             A root process of it may still be running (sudo cannot pass the stop on): look for \
             it with `ps` and stop it before another local build",
            BUILD_TIMEOUT.as_secs() / 3600,
            log_path.display()
        )));
    };
    local_outcome(code, log_path, &spec)?;
    if fresh_rpm_count(&local_pkgs, started) == 0 {
        return Err(Error::NotActionable(format!(
            "`osc build` of {pkg_name} exited 0 but wrote no .rpm under {}",
            local_pkgs.display()
        )));
    }
    Ok(())
}

/// The build step: `osc build` here when `local` is set, else a build on OBS
/// (committed by the caller already) waited for and fetched. Either way the log
/// is at `log_path` and the RPMs are in `rpms_dir`.
#[allow(clippy::too_many_arguments)]
pub fn build_package(
    local: Option<&LocalBuild>,
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    checkout: &Path,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    match local {
        Some(settings) => local_build_package(
            settings, checkout, pkg_name, repo, arch, log_path, rpms_dir, emit,
        ),
        None => obs_build(project, pkg_name, repo, arch, log_path, rpms_dir, emit),
    }
}
