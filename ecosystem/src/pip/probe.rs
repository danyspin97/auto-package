use super::*;

/// The config files pip may read, as paths, in the fixed order their digests
/// are hashed. A slot that does not apply (unset, relative) is `None`, so the
/// position of every file stays the same whatever else is set:
///
/// 1. `PIP_CONFIG_FILE`, when absolute;
/// 2. `$XDG_CONFIG_HOME/pip/pip.conf`, when absolute;
/// 3. `~/.config/pip/pip.conf` and 4. `~/.pip/pip.conf`;
/// 5. `/etc/pip.conf`, 6. `/etc/xdg/pip/pip.conf`, and the
///    `/usr/local/etc/xdg` and `/usr/etc/xdg` copies that `pip config debug`
///    lists for a distribution-built pip;
/// 7. `pip/pip.conf` under each absolute `$XDG_CONFIG_DIRS` entry (`/etc/xdg`
///    when the variable is unset or empty);
/// 8. `<prefix>/pip.conf`, pip's site file, when the interpreter's prefix is
///    known.
///
/// The sandbox binds only some of these (`ETC_ENTRIES`, the user's config
/// files); hashing the rest is harmless, a file that changes just changes the
/// key. A file that is missing or not a regular file hashes as `None`.
pub(super) fn config_candidates(
    vars: &BTreeMap<OsString, OsString>,
    prefix: Option<&std::path::Path>,
) -> Vec<Option<std::path::PathBuf>> {
    use std::path::PathBuf;
    let path = |key: &str| {
        vars.get(OsStr::new(key))
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = path("HOME").unwrap_or_else(|| "/nonexistent".into());
    let xdg_home = path("XDG_CONFIG_HOME").filter(|p| p.is_absolute());
    let xdg_dirs: Vec<PathBuf> = match path("XDG_CONFIG_DIRS") {
        Some(dirs) => dirs
            .to_string_lossy()
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .collect(),
        None => vec![PathBuf::from("/etc/xdg")],
    };
    let mut out = vec![
        path("PIP_CONFIG_FILE").filter(|p| p.is_absolute()),
        xdg_home.map(|x| x.join("pip/pip.conf")),
        Some(home.join(".config/pip/pip.conf")),
        Some(home.join(".pip/pip.conf")),
        Some("/etc/pip.conf".into()),
        Some("/etc/xdg/pip/pip.conf".into()),
        Some("/usr/local/etc/xdg/pip/pip.conf".into()),
        Some("/usr/etc/xdg/pip/pip.conf".into()),
    ];
    out.extend(
        xdg_dirs
            .into_iter()
            .map(|dir| dir.is_absolute().then(|| dir.join("pip/pip.conf"))),
    );
    out.push(prefix.map(|p| p.join("pip.conf")));
    out
}

/// The interpreter prefix (`sys.prefix`, approximately) of `program`, found on
/// `path` without running anything: the parent of the `bin` directory it sits
/// in, or its own directory otherwise. This is a best-effort guess; a user
/// install (`~/.local/bin/pip`) really has the system prefix, and a symlinked
/// interpreter is taken where it is found. Either way the result only decides
/// which `pip.conf` is hashed into the key.
pub(super) fn interpreter_prefix_of(program: &str, path: &OsStr) -> Option<std::path::PathBuf> {
    let file = if program.contains('/') {
        std::path::PathBuf::from(program)
    } else {
        std::env::split_paths(path)
            .filter(|d| d.is_absolute())
            .map(|d| d.join(program))
            .find(|p| p.is_file())?
    };
    let dir = file.parent()?;
    if dir.file_name() == Some(OsStr::new("bin")) {
        dir.parent().map(|p| p.to_path_buf())
    } else {
        Some(dir.to_path_buf())
    }
}

/// Host Python interpreter identifier, from `python3 --version` (e.g.
/// `Python 3.13.1`). Falls back to the empty string if the probe fails — a
/// missing interpreter is a bigger problem than a slightly less specific cache
/// key, and `resolve` itself will fail loudly on the subsequent `pip` call.
fn host_python() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| probe_python("python3")).clone()
}

/// The `--version` line of `program`, or the empty string when it cannot run.
pub(super) fn probe_python(program: &str) -> String {
    Command::new(program)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            // Older `python3 --version` prints to stderr; newer to stdout.
            let mut out = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if out.is_empty() {
                out = String::from_utf8_lossy(&o.stderr).trim().to_string();
            }
            out
        })
        .unwrap_or_default()
}

/// The interpreter the default `pip` on `PATH` belongs to, as `Python 3.N`, from
/// `pip --version`. Probed once per process. Falls back to the host's
/// `python3` when the probe cannot run or its output cannot be parsed.
pub(super) fn default_python() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| probe_default_pip().unwrap_or_else(host_python))
        .clone()
}

/// Upper bound on the `pip --version` probe. It runs on the host, unsandboxed
/// like the other interpreter probes, but pip is not to be left to hang.
const PIP_VERSION_TIMEOUT: Duration = Duration::from_secs(60);

fn probe_default_pip() -> Option<String> {
    let mut cmd = Command::new("pip");
    cmd.arg("--version");
    let out = run_with_timeout(cmd, PIP_VERSION_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    parse_pip_version(&String::from_utf8_lossy(&out.stdout))
}

/// The `Python 3.N` interpreter named by `pip --version` output, e.g.
/// `pip 24.0 from /usr/lib/python3/dist-packages/pip (python 3.12)`. `None`
/// when there is no `(python 3.N)` part.
pub(super) fn parse_pip_version(out: &str) -> Option<String> {
    let rest = out.split("(python ").nth(1)?;
    let version: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = version.split('.');
    let (major, minor) = (parts.next()?, parts.next()?);
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (numeric(major) && numeric(minor)).then(|| format!("Python {major}.{minor}"))
}

/// Host platform tag, from `sysconfig.get_platform()` (e.g.
/// `linux-x86_64`). Falls back to `std::env::consts::OS-ARCH` when the Python
/// probe can't run — enough to keep an x86_64 cache from being reused on
/// aarch64 even if `python3` is missing entirely.
pub(super) fn host_platform() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(probe_host_platform).clone()
}

fn probe_host_platform() -> String {
    let probed = Command::new("python3")
        .args(["-c", "import sysconfig; print(sysconfig.get_platform())"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    probed.unwrap_or_else(|| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}
