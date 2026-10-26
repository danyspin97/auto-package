//! The pip dependency resolver.
//!
//! `pip install --dry-run --report -` resolves a package's full runtime
//! closure without installing anything. The report is JSON (PEP 723 report
//! format v1) and contains each node's name, version, sha256, and
//! `requires_dist` with markers unevaluated.
//!
//! Resolution may build sdists, and building runs the sdist's build backend:
//! arbitrary code from the index. So pip runs inside a bubblewrap sandbox
//! (see [`crate::pip_sandbox`]) with a cleared environment that holds only the
//! allowlisted variables (`PIP_ENV_ALLOWLIST`): the backend sees neither the
//! user's files nor OBS, GitHub or Anthropic credentials, and cannot write
//! outside scratch space. The network stays open, as resolution needs it.
//!
//! Without a working sandbox pip is not run on the host. Resolution falls
//! back to wheels only (`--only-binary=:all:`), and [`set_unsafe_resolve`]
//! (`--unsafe-resolve`, or `AUTO_PACKAGE_UNSAFE_RESOLVE=1`) opts into running
//! pip unsandboxed. The same switch turns the sandbox off where it works.
//!
//! Known limitation: `--python-version` is ignored by pip when a real
//! interpreter is running — marker evaluation uses the host interpreter.
//! Cross-flavor resolution (python311 vs python313) requires the actual
//! interpreter binaries to be installed and a separate run per flavor.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pip_sandbox::{self, SandboxError};

/// One package in the resolved closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// Raw `Requires-Dist` lines from the wheel metadata, markers and extras
    /// unevaluated. Empty when the package declares no runtime dependencies.
    pub requires_dist: Vec<String>,
    pub requires_python: Option<String>,
    /// The `requested` flag is true only for the root package — pip sets it to
    /// distinguish what you asked for from what the solver pulled in.
    pub is_requested: bool,
    pub is_yanked: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run pip: {0}")]
    Launch(std::io::Error),

    #[error("package not found on PyPI: {0}")]
    NotFound(String),

    /// Wheels-only resolution (no sandbox, no opt-in) found nothing. The
    /// release may exist as an sdist only.
    #[error(
        "no wheel found for {0}; it may ship only an sdist, which is not built because \
         there is no sandbox to build it in (install bubblewrap and enable unprivileged \
         user namespaces, or pass --unsafe-resolve to build it unsandboxed)"
    )]
    NoWheel(String),

    #[error(transparent)]
    Sandbox(#[from] SandboxError),

    /// Every release exists but pip skipped all of them because their
    /// `Requires-Python` excludes the interpreter pip runs under.
    #[error(
        "{name} is on PyPI, but no release supports the Python that pip runs under \
         (e.g. {example}); put a pip for an older/newer Python first on PATH"
    )]
    IncompatiblePython { name: String, example: String },

    #[error("pip exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("pip produced malformed JSON: {0}")]
    Malformed(#[from] serde_json::Error),

    /// pip resolved the package but couldn't find a sha256 for a node. This
    /// indicates a corrupted or non-standard index, not a usage error.
    #[error("no sha256 for {0}")]
    NoDigest(String),
}

/// Get the cache directory for pip resolution results.
///
/// Caches are stored in `~/.cache/auto-package/pip-resolve/` by default,
/// or `$XDG_CACHE_HOME/auto-package/pip-resolve/` if XDG_CACHE_HOME is set.
fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".cache")
        });
    base.join("auto-package").join("pip-resolve")
}

/// Get the cache file path for a specific package resolution.
///
/// Delegates to [`cache_key`], which also folds in the host interpreter,
/// platform, resolve mode and the environment pip inherits, so a cache
/// populated under one Python/arch, one mode (sdists allowed or not) or one
/// index is not reused under another. The old narrow key
/// (`<name>-<version>.json`) is simply never matched; the existing TTL prunes
/// those stale files.
///
/// `python` is the version string of the interpreter whose pip produced the
/// closure (see [`resolve`]), not necessarily the host's. `prefix` is that
/// interpreter's prefix when it is known (see [`interpreter_prefix_of`]).
fn cache_path(
    dir: &std::path::Path,
    name: &str,
    version: Option<&str>,
    python: &str,
    prefix: Option<&std::path::Path>,
    mode: ResolveMode,
) -> std::path::PathBuf {
    let vars: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    dir.join(cache_key(
        name,
        version,
        python,
        &host_platform(),
        mode,
        &vars,
        prefix,
    ))
}

/// Build the cache filename for a resolve result.
///
/// `pip install --dry-run` resolves against the host interpreter and host
/// platform regardless of our `--arch`/`--distro`: `--python-version` is
/// cosmetic once a real interpreter is running, and `pip` has no `--platform`
/// knob that works in the default (non-wheel-download) mode. Two concurrent
/// runs on different Python majors or different arches must not share a
/// cached result, so interpreter + platform go into the key as a short hash.
///
/// The resolve mode goes in too: a wheels-only closure lacks sdist-only
/// releases and dependencies, and a closure resolved with sdists may contain
/// releases a wheels-only run must reject, so the modes never share an entry.
///
/// `vars` is the environment pip would inherit. Its index, config, CA and
/// proxy variables (see [`RESOLUTION_ENV`]) and the config files pip reads
/// change which closure comes back, so they are folded in too. Only digests
/// go into the hash: no URL, credential or path is stored or printed.
pub(crate) fn cache_key(
    name: &str,
    version: Option<&str>,
    python: &str,
    platform: &str,
    mode: ResolveMode,
    vars: &BTreeMap<OsString, OsString>,
    prefix: Option<&std::path::Path>,
) -> String {
    let configs = config_candidates(vars, prefix);
    cache_key_with_configs(name, version, python, platform, mode, vars, &configs)
}

/// [`cache_key`] over an explicit list of config candidates (see
/// [`config_candidates`]). Split out so tests can inject the system paths.
fn cache_key_with_configs(
    name: &str,
    version: Option<&str>,
    python: &str,
    platform: &str,
    mode: ResolveMode,
    vars: &BTreeMap<OsString, OsString>,
    configs: &[Option<std::path::PathBuf>],
) -> String {
    let norm_name = normalize_name(name);
    let mut hasher = Sha256::new();
    hasher.update(python.as_bytes());
    hasher.update([0u8]);
    hasher.update(platform.as_bytes());
    hasher.update([0u8]);
    hasher.update(mode.cache_tag().as_bytes());
    // The raw name/version go into the hash too, so two inputs that
    // sanitize to the same filename part still get distinct keys.
    hasher.update([0u8]);
    hasher.update(norm_name.as_bytes());
    hasher.update([0u8]);
    hasher.update(version.unwrap_or("").as_bytes());
    // Fixed order (the constant's, then the config slots), so the same inputs
    // always hash the same. An unset variable and an empty one differ.
    for var in RESOLUTION_ENV {
        hasher.update(var.as_bytes());
        hasher.update([0u8]);
        let value = vars
            .get(OsStr::new(var))
            .map(|v| sha256(v.as_encoded_bytes()));
        hash_field(&mut hasher, value.as_deref());
    }
    for config in config_digests(configs) {
        hash_field(&mut hasher, config.as_deref());
    }
    let digest = hex::encode(hasher.finalize());
    let short = &digest[..12];
    let base = match version {
        Some(v) => format!("{}-{}", filename_safe(&norm_name), filename_safe(v)),
        None => format!("{}-latest", filename_safe(&norm_name)),
    };
    format!("{base}-{short}.json")
}

/// Environment variables pip inherits that can change what it resolves: the
/// index, extra indexes, trusted hosts and config file, and the CA and proxy
/// settings that decide which server answers. Every name here must also be in
/// [`PIP_ENV_ALLOWLIST`], or pip never sees it.
const RESOLUTION_ENV: &[&str] = &[
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "PIP_TRUSTED_HOST",
    "PIP_CONFIG_FILE",
    "PIP_CERT",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

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
fn config_candidates(
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
fn interpreter_prefix_of(program: &str, path: &OsStr) -> Option<std::path::PathBuf> {
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

/// Digests of the config files in `candidates` (see [`config_candidates`]), in
/// order. A candidate that is absent or not a regular file is `None`.
fn config_digests(candidates: &[Option<std::path::PathBuf>]) -> Vec<Option<Vec<u8>>> {
    candidates
        .iter()
        .map(|candidate| {
            let file = candidate.as_ref().filter(|p| p.is_file())?;
            std::fs::read(file).ok().map(|bytes| sha256(&bytes))
        })
        .collect()
}

/// SHA-256 of `bytes`, as raw digest bytes.
fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// Feed one optional digest into `hasher`, tagged so `None` and `Some` never
/// produce the same bytes. Digests are fixed-length, so no framing is needed.
fn hash_field(hasher: &mut Sha256, field: Option<&[u8]>) {
    match field {
        Some(bytes) => {
            hasher.update([1u8]);
            hasher.update(bytes);
        }
        None => hasher.update([0u8]),
    }
}

/// PEP 503 normalisation: runs of `-`, `_`, `.` become one `-`, lowercased.
fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
            continue;
        }
        if sep {
            out.push('-');
            sep = false;
        }
        out.extend(c.to_lowercase());
    }
    if sep {
        out.push('-');
    }
    out
}

/// Replace everything outside `[a-z0-9.+-]` with `_` so untrusted input can
/// never contain a path separator (and `..` is defused by the hash suffix
/// and the fixed `-`-joined shape: the result is only ever a filename part).
fn filename_safe(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '.' | '+' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    t.replace("..", "._")
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
fn probe_python(program: &str) -> String {
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
fn default_python() -> String {
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
fn parse_pip_version(out: &str) -> Option<String> {
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
fn host_platform() -> String {
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

/// Load cached resolution result if available and fresh.
///
/// Cache is considered stale if older than 7 days (for pinned versions) or
/// 1 day (for "latest").
fn load_cached_resolve(
    dir: &std::path::Path,
    name: &str,
    version: Option<&str>,
    python: &str,
    prefix: Option<&std::path::Path>,
    mode: ResolveMode,
) -> Option<Vec<Node>> {
    let path = cache_path(dir, name, version, python, prefix, mode);

    // Check if cache file exists
    let metadata = std::fs::metadata(&path).ok()?;

    // Determine freshness threshold
    let max_age_secs = match version {
        Some(_) => 7 * 24 * 60 * 60, // 7 days for pinned versions
        None => 24 * 60 * 60,        // 1 day for "latest"
    };

    // Check if cache is fresh
    let age = metadata.modified().ok()?.elapsed().ok()?.as_secs();

    if age > max_age_secs {
        let _ = std::fs::remove_file(&path);
        ap_core::diag!(
            "  cache entry stale (age: {} days), re-resolving",
            age / 86400
        );
        return None;
    }

    // Load and parse cache
    let parsed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str::<Vec<Node>>(&c).ok());
    let Some(nodes) = parsed else {
        // Corrupt or unreadable: drop it so the next run re-resolves cleanly.
        let _ = std::fs::remove_file(&path);
        return None;
    };

    ap_core::diag!(
        "  ✓ loaded resolution from cache ({} packages)",
        nodes.len()
    );
    Some(nodes)
}

/// Save resolution result to cache, keyed by the interpreter that produced it.
fn save_cached_resolve(
    dir: &std::path::Path,
    name: &str,
    version: Option<&str>,
    python: &str,
    prefix: Option<&std::path::Path>,
    mode: ResolveMode,
    nodes: &[Node],
) {
    let path = cache_path(dir, name, version, python, prefix, mode);

    // Create cache directory if it doesn't exist
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Serialize and save
    // Write to a temp file in the same directory and rename into place, so
    // a concurrent reader never sees a half-written cache file.
    if let Ok(json) = serde_json::to_string_pretty(nodes)
        && ap_core::fsx::write_atomic(&path, json).is_ok()
    {
        ap_core::diag!("  cached resolution for future use");
    }
}

/// Resolve the runtime closure of `name` at `version`.
///
/// `version` may be `None` for "latest". Returns all nodes including the root
/// package itself; callers can filter by `Node::is_requested` if they only
/// want transitive deps.
///
/// Results are cached in `~/.cache/auto-package/pip-resolve/`. Cached results
/// for pinned versions are valid for 7 days, "latest" queries for 1 day.
/// Turn a failed `pip install --dry-run` into the most specific error.
///
/// pip says "No matching distribution found" both when the project does not
/// exist and when every release was skipped for its `Requires-Python`; only
/// the preceding "Ignored the following versions…" line tells them apart.
fn classify_failure(name: &str, code: i32, stderr: String, wheels_only: bool) -> Error {
    if let Some(line) = stderr.lines().find(|l| {
        l.contains("Ignored the following versions that require a different python version")
    }) {
        let example = line
            .split("version: ")
            .nth(1)
            .and_then(|l| l.split("; ").next())
            .unwrap_or("all releases")
            .to_string();
        return Error::IncompatiblePython {
            name: name.to_string(),
            example,
        };
    }
    if stderr.contains("Could not find a version that satisfies the requirement")
        || stderr.contains("No matching distribution found for")
    {
        // Without sdists this also covers releases that ship only an sdist.
        return if wheels_only {
            Error::NoWheel(name.to_string())
        } else {
            Error::NotFound(name.to_string())
        };
    }
    Error::Failed { code, stderr }
}

/// Interpreters tried, in order, when the default pip's Python is outside a
/// project's `Requires-Python`. Stable releases first, newest-compatible
/// guesses before older ones.
const ALTERNATE_PYTHON_MINORS: [u32; 5] = [12, 11, 13, 10, 14];

/// Run `pip install --dry-run --report -` through `program` (a pip command
/// line prefix) and return its JSON report.
fn run_pip_report(
    program: &[&str],
    name: &str,
    spec: &str,
    mode: ResolveMode,
) -> Result<Vec<u8>, Error> {
    let mut cmd = match mode {
        ResolveMode::Sandboxed => {
            let netrc = std::env::var_os(pip_sandbox::NETRC_ENV).is_some_and(|v| v == "1");
            sandboxed_command(program, std::env::vars_os(), netrc)?
        }
        ResolveMode::WheelsOnly | ResolveMode::Unsandboxed => {
            pip_command(program, std::env::vars_os())
        }
    };
    cmd.args(pip_report_args(spec, mode == ResolveMode::WheelsOnly));
    let out = run_with_timeout(cmd, PIP_TIMEOUT)?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(classify_failure(
            name,
            code,
            stderr,
            mode == ResolveMode::WheelsOnly,
        ));
    }
    Ok(out.stdout)
}

/// How pip is run for a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveMode {
    /// Inside bubblewrap, sdists allowed. The default when bwrap works.
    Sandboxed,
    /// On the host, wheels only: no sandbox is available and the unsafe
    /// opt-in was not given, so no build backend may run.
    WheelsOnly,
    /// On the host, sdists allowed, because the user asked for it.
    Unsandboxed,
}

impl ResolveMode {
    /// Stable label for this mode, folded into the resolution cache key.
    fn cache_tag(self) -> &'static str {
        match self {
            ResolveMode::Sandboxed => "sandboxed",
            ResolveMode::WheelsOnly => "wheels-only",
            ResolveMode::Unsandboxed => "unsandboxed",
        }
    }

    /// What the user should be told about this mode, when it is not the
    /// ordinary sandboxed one.
    pub fn notice(self) -> Option<String> {
        match self {
            ResolveMode::Sandboxed => None,
            ResolveMode::WheelsOnly => Some(
                "bubblewrap is not usable here (bwrap missing, or unprivileged user \
                 namespaces are disabled), so pip resolves wheels only and releases that \
                 ship only an sdist will fail to resolve. Building an sdist runs its \
                 build backend on your machine; to allow that anyway, pass \
                 --unsafe-resolve (or set AUTO_PACKAGE_UNSAFE_RESOLVE=1)."
                    .to_string(),
            ),
            ResolveMode::Unsandboxed => Some(
                "--unsafe-resolve: pip runs without a sandbox and may build sdists. \
                 Their build backends run as you, with access to your files and network."
                    .to_string(),
            ),
        }
    }
}

static UNSAFE_RESOLVE: AtomicBool = AtomicBool::new(false);

/// Environment variable equivalent to `--unsafe-resolve`.
pub const UNSAFE_RESOLVE_ENV: &str = "AUTO_PACKAGE_UNSAFE_RESOLVE";

/// Allow (or stop allowing) resolution to run pip without the sandbox.
pub fn set_unsafe_resolve(unsafe_resolve: bool) {
    UNSAFE_RESOLVE.store(unsafe_resolve, Ordering::Relaxed);
}

fn unsafe_resolve() -> bool {
    UNSAFE_RESOLVE.load(Ordering::Relaxed)
        || std::env::var_os(UNSAFE_RESOLVE_ENV).is_some_and(|v| v == "1")
}

/// How resolution runs now: unsandboxed on request, sandboxed when bwrap
/// works here (probed once per process), wheels only otherwise.
pub fn resolve_mode() -> ResolveMode {
    if unsafe_resolve() {
        ResolveMode::Unsandboxed
    } else if ap_core::sandbox::bwrap_works() {
        ResolveMode::Sandboxed
    } else {
        ResolveMode::WheelsOnly
    }
}

/// `program` run through bubblewrap, with a cleared environment for bwrap
/// itself and the sandbox's own environment built from the allowlisted
/// variables found in `vars`.
fn sandboxed_command(
    program: &[&str],
    vars: impl Iterator<Item = (OsString, OsString)>,
    netrc: bool,
) -> Result<Command, Error> {
    let bwrap = ap_core::sandbox::bwrap_path().ok_or_else(|| {
        Error::Launch(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "bwrap not found on PATH",
        ))
    })?;
    let vars: Vec<_> = vars.collect();
    let var = |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let host = pip_sandbox::Host {
        home: var("HOME")
            .map(Into::into)
            .unwrap_or_else(|| "/nonexistent".into()),
        path: var("PATH").unwrap_or_default(),
        vars: pip_env(vars.into_iter()),
        netrc,
    };
    let args = match pip_sandbox::bwrap_args(program, &host) {
        Ok(args) => args,
        // A missing interpreter is how the alternate-Python search moves on.
        // An unsafe prefix stays a Sandbox error, which `select_candidate`
        // also moves on from.
        Err(SandboxError::ProgramNotFound(p)) => {
            return Err(Error::Launch(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{p} not found on PATH"),
            )));
        }
        Err(e) => return Err(e.into()),
    };
    let mut cmd = Command::new(bwrap);
    cmd.env_clear().args(args);
    Ok(cmd)
}

/// Variables pip is given. Everything else is cleared. Proxy, CA and index
/// settings are needed to reach the index; none of these carry credentials
/// for OBS, GitHub or Anthropic.
const PIP_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "PIP_TRUSTED_HOST",
    "PIP_CONFIG_FILE",
    "PIP_CERT",
];

/// A `program` command line prefix with a cleared environment that holds only
/// the allowlisted variables found in `vars`.
fn pip_command(program: &[&str], vars: impl Iterator<Item = (OsString, OsString)>) -> Command {
    let mut cmd = Command::new(program[0]);
    cmd.env_clear().envs(pip_env(vars)).args(&program[1..]);
    cmd
}

/// The `pip install` arguments for one resolution. Sdists are not excluded
/// unless `wheels_only`, so pip can resolve a release that ships only an
/// sdist.
fn pip_report_args(spec: &str, wheels_only: bool) -> Vec<&str> {
    let mut args = vec![
        "install",
        "--dry-run",
        "--ignore-installed",
        "--quiet",
        "--disable-pip-version-check",
        "--report",
        "-",
    ];
    if wheels_only {
        args.push("--only-binary=:all:");
    }
    // End of options: a hostile `name` must not be parsed as a flag.
    args.push("--");
    args.push(spec);
    args
}

/// The subset of `vars` pip may inherit.
fn pip_env(vars: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.filter(|(k, _)| k.to_str().is_some_and(|k| PIP_ENV_ALLOWLIST.contains(&k)))
        .collect()
}

/// Wall-clock limit for one pip resolve.
const PIP_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Most of one stream kept from pip. A report is a few megabytes at the very
/// most; a runaway child must not fill memory.
const MAX_PIP_OUTPUT: u64 = 64 * 1024 * 1024;

/// How long the output readers get once the child has exited.
const READER_GRACE: Duration = Duration::from_secs(5);

/// `Command::output` with a deadline: the child is killed on expiry.
///
/// The child gets its own process group so that a timeout (or a descendant
/// left running after the child exits) kills the whole tree; a grandchild
/// holding the pipes open would otherwise block the readers forever.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<std::process::Output, Error> {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::Launch)?;
    // Drain both pipes on threads so a chatty child can't block on a full pipe.
    // A read error is sent back rather than dropped: truncated output must
    // fail the call, not parse as a shorter (wrong) report.
    fn drain(
        r: Option<impl Read + Send + 'static>,
    ) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = match r {
                Some(mut r) => {
                    let mut buf = Vec::new();
                    (&mut r)
                        .take(MAX_PIP_OUTPUT)
                        .read_to_end(&mut buf)
                        .and_then(|_| {
                            // Keep reading past the cap, so the child never
                            // blocks on a full pipe, but report it: a cut-off
                            // report must fail, not parse as a shorter one.
                            let extra = std::io::copy(&mut r, &mut std::io::sink())?;
                            if extra > 0 {
                                return Err(std::io::Error::other(format!(
                                    "more than {MAX_PIP_OUTPUT} bytes"
                                )));
                            }
                            Ok(buf)
                        })
                }
                None => Ok(Vec::new()),
            };
            let _ = tx.send(res);
        });
        rx
    }
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());
    let start = Instant::now();
    let deadline = start + timeout;
    // The exit is noticed without reaping the child, and its group is swept
    // before it is reaped: until then the child still owns its pid, so the
    // sweep cannot hit an unrelated group that was handed the number.
    let status = loop {
        match ap_core::pgroup::has_exited(child.id()) {
            // Reap whatever it left behind so the pipes close.
            Ok(true) => {
                ap_core::pgroup::kill_group(child.id());
                match child.wait() {
                    Ok(st) => break st,
                    Err(e) => return Err(Error::Launch(e)),
                }
            }
            Ok(false) if Instant::now() >= deadline => {
                ap_core::pgroup::kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Failed {
                    code: -1,
                    stderr: format!("timed out after {}s and was killed", timeout.as_secs()),
                });
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                ap_core::pgroup::kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Launch(e));
            }
        }
    };
    // One shared grace period for both readers.
    let reader_deadline = Instant::now() + READER_GRACE;
    let collect = |rx: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        let wait = reader_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Ok(buf)) => Ok(buf),
            Ok(Err(e)) => Err(format!("could not read pip output: {e}")),
            Err(_) => Err("pip output did not close after the process exited".to_string()),
        }
    };
    let stdout = collect(out_rx);
    let stderr = collect(err_rx);
    match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => Ok(std::process::Output {
            status,
            stdout,
            stderr,
        }),
        (Err(e), _) | (_, Err(e)) => Err(Error::Failed {
            code: status.code().unwrap_or(-1),
            stderr: e,
        }),
    }
}

/// Pick the pip whose report is used: the default pip's, or else the first of
/// `alternates` whose pip works. Returns the alternate used (`None` for the
/// default) with the report.
///
/// The default is passed over when its Python is outside every release's
/// `Requires-Python` ([`Error::IncompatiblePython`]) or its prefix may not be
/// sandboxed ([`SandboxError::UnsafePrefix`]): another interpreter may be fine.
/// Any other default failure is final. Alternates that cannot launch or fail
/// either of those ways are skipped; other alternate failures are surfaced.
/// When nothing works, the default's error is returned.
fn select_candidate(
    first: Result<Vec<u8>, Error>,
    alternates: &[String],
    mut attempt: impl FnMut(&str) -> Result<Vec<u8>, Error>,
) -> Result<(Option<String>, Vec<u8>), Error> {
    let e = match first {
        Ok(out) => return Ok((None, out)),
        Err(e) if is_candidate_skip(&e) => e,
        Err(e) => return Err(e),
    };
    let mut last_err: Option<Error> = None;
    for py in alternates {
        match attempt(py) {
            Ok(out) => return Ok((Some(py.clone()), out)),
            // A missing interpreter or another incompatibility says
            // nothing new; anything else is worth surfacing.
            Err(Error::Launch(_) | Error::IncompatiblePython { .. }) => {}
            Err(Error::Sandbox(SandboxError::UnsafePrefix { .. })) => {}
            Err(other) => last_err = Some(other),
        }
    }
    match last_err {
        Some(other) => Err(Error::Failed {
            code: -1,
            stderr: format!("{e}; alternate interpreters also failed: {other}"),
        }),
        None => Err(e),
    }
}

/// Errors that mean "this interpreter is not the one to use", not "the
/// project cannot be resolved".
fn is_candidate_skip(e: &Error) -> bool {
    matches!(
        e,
        Error::IncompatiblePython { .. } | Error::Sandbox(SandboxError::UnsafePrefix { .. })
    )
}

pub fn resolve(name: &str, version: Option<&str>) -> Result<Vec<Node>, Error> {
    // The mode is part of the cache key, so it is decided before the cache
    // is read.
    let mode = resolve_mode();
    let dir = cache_dir();
    // The default pip runs whatever `pip` is on PATH, which may not belong to
    // the host's `python3`: the key names the interpreter it reports.
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let default_py = default_python();
    let default_prefix = interpreter_prefix_of("pip", &path_var);

    // Try to load from cache first. Only entries recorded under the default
    // pip's interpreter are read: an entry produced by an alternate interpreter
    // is stored under that interpreter's key, so it is never served to the
    // default pip.
    if let Some(cached) = load_cached_resolve(
        &dir,
        name,
        version,
        &default_py,
        default_prefix.as_deref(),
        mode,
    ) {
        return Ok(cached);
    }

    ap_core::diag!("  resolving dependencies with pip (not in cache)...");

    let spec = match version {
        Some(v) => format!("{name}=={v}"),
        None => name.to_string(),
    };

    // The default pip's interpreter may be outside every release's
    // Requires-Python (open-webui needs <3.13 on a 3.13 system), or the pip on
    // PATH may sit where it cannot be sandboxed: the project is fine, so try
    // another installed Python.
    let alternates: Vec<String> = ALTERNATE_PYTHON_MINORS
        .iter()
        .map(|minor| format!("python3.{minor}"))
        .collect();
    let (alternate, stdout) = select_candidate(
        run_pip_report(&["pip"], name, &spec, mode),
        &alternates,
        |py| run_pip_report(&[py, "-m", "pip"], name, &spec, mode),
    )?;
    // The interpreter whose pip produced `stdout`: the default pip's, or the
    // alternate that was used. The cache entry is keyed by it.
    let (produced_by, produced_prefix) = match &alternate {
        Some(py) => (probe_python(py), interpreter_prefix_of(py, &path_var)),
        None => (default_py, default_prefix),
    };
    if let Some(py) = alternate {
        ap_core::diag!("  default pip was not usable for {name}; resolved with {py}");
    }

    let report: Report = serde_json::from_slice(&stdout)?;
    let nodes: Vec<Node> = report
        .install
        .into_iter()
        .map(|e| {
            let sha256 = e
                .download_info
                .archive_info
                .hashes
                .sha256
                .ok_or_else(|| Error::NoDigest(e.metadata.name.clone()))?;
            Ok::<Node, Error>(Node {
                name: e.metadata.name,
                version: e.metadata.version,
                sha256,
                requires_dist: e.metadata.requires_dist,
                requires_python: e.metadata.requires_python,
                is_requested: e.requested,
                is_yanked: e.is_yanked,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    // Save to cache for future use. A result whose interpreter version cannot
    // be probed is not cached: there is no key that would describe it.
    if !produced_by.is_empty() {
        save_cached_resolve(
            &dir,
            name,
            version,
            &produced_by,
            produced_prefix.as_deref(),
            mode,
            &nodes,
        );
    }

    Ok(nodes)
}

// --- wire format -----------------------------------------------------------

#[derive(Deserialize)]
struct Report {
    install: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    download_info: RawDownloadInfo,
    #[serde(default)]
    is_yanked: bool,
    #[serde(default)]
    requested: bool,
    metadata: RawMetadata,
}

#[derive(Deserialize)]
struct RawDownloadInfo {
    archive_info: RawArchiveInfo,
}

#[derive(Deserialize)]
struct RawArchiveInfo {
    hashes: RawHashes,
}

#[derive(Deserialize, Default)]
struct RawHashes {
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct RawMetadata {
    name: String,
    version: String,
    #[serde(default)]
    requires_dist: Vec<String>,
    #[serde(default)]
    requires_python: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `pip install --dry-run --report -` on httpx 0.28.1.
    /// The wheel URL and long description are dropped; every other field is
    /// verbatim, including the empty `requires_dist` on leaf packages.
    const HTTPX_REPORT: &str = r#"{
      "version": "1",
      "pip_version": "26.2",
      "install": [
        {
          "download_info": {
            "url": "https://files.pythonhosted.org/packages/httpx-0.28.1-py3-none-any.whl",
            "archive_info": {
              "hashes": {"sha256": "d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"},
              "hash": "sha256=d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"
            }
          },
          "is_direct": false, "is_yanked": false, "requested": true,
          "metadata": {
            "metadata_version": "2.3", "name": "httpx", "version": "0.28.1",
            "requires_python": ">=3.8",
            "requires_dist": [
              "anyio", "certifi", "httpcore==1.*", "idna",
              "brotli; (platform_python_implementation == 'CPython') and extra == 'brotli'",
              "click==8.*; extra == 'cli'", "h2<5,>=3; extra == 'http2'"
            ]
          }
        },
        {
          "download_info": {
            "url": "https://files.pythonhosted.org/packages/h11-0.16.0-py3-none-any.whl",
            "archive_info": {
              "hashes": {"sha256": "63cf8bbe7c1b3e2c9a6b0f3c78b876c5c9def79d5a519da18aae9c1df394b498"},
              "hash": "sha256=63cf8bbe7c1b3e2c9a6b0f3c78b876c5c9def79d5a519da18aae9c1df394b498"
            }
          },
          "is_direct": false, "is_yanked": false, "requested": false,
          "metadata": {
            "metadata_version": "2.1", "name": "h11", "version": "0.16.0",
            "requires_python": ">=3.8",
            "requires_dist": []
          }
        },
        {
          "download_info": {
            "url": "https://files.pythonhosted.org/packages/certifi-2026.7.22-py3-none-any.whl",
            "archive_info": {
              "hashes": {"sha256": "abc123"},
              "hash": "sha256=abc123"
            }
          },
          "is_direct": false, "is_yanked": false, "requested": false,
          "metadata": {
            "metadata_version": "2.1", "name": "certifi", "version": "2026.7.22"
          }
        }
      ],
      "environment": {
        "python_version": "3.13"
      }
    }"#;

    fn parse(json: &str) -> Vec<Node> {
        let report: Report = serde_json::from_str(json).unwrap();
        report
            .install
            .into_iter()
            .map(|e| {
                let sha256 = e
                    .download_info
                    .archive_info
                    .hashes
                    .sha256
                    .expect("fixture has sha256");
                Node {
                    name: e.metadata.name,
                    version: e.metadata.version,
                    sha256,
                    requires_dist: e.metadata.requires_dist,
                    requires_python: e.metadata.requires_python,
                    is_requested: e.requested,
                    is_yanked: e.is_yanked,
                }
            })
            .collect()
    }

    #[test]
    fn parses_root_and_transitive_nodes() {
        let nodes = parse(HTTPX_REPORT);
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn root_is_marked_requested() {
        let nodes = parse(HTTPX_REPORT);
        let root = nodes.iter().find(|n| n.name == "httpx").unwrap();
        assert!(root.is_requested);
        assert!(!nodes.iter().find(|n| n.name == "h11").unwrap().is_requested);
    }

    #[test]
    fn markers_and_extras_are_kept_unevaluated() {
        let nodes = parse(HTTPX_REPORT);
        let root = nodes.iter().find(|n| n.name == "httpx").unwrap();
        assert!(
            root.requires_dist
                .iter()
                .any(|r| r.contains("extra == 'cli'"))
        );
        assert!(
            root.requires_dist
                .iter()
                .any(|r| r.contains("platform_python_implementation"))
        );
    }

    #[test]
    fn leaf_with_no_requires_dist_field_gives_empty_vec() {
        // `certifi` in the fixture has no `requires_dist` key at all.
        let nodes = parse(HTTPX_REPORT);
        let leaf = nodes.iter().find(|n| n.name == "certifi").unwrap();
        assert!(leaf.requires_dist.is_empty());
    }

    #[test]
    fn sha256_is_extracted_from_hashes_not_the_legacy_hash_field() {
        let nodes = parse(HTTPX_REPORT);
        assert_eq!(
            nodes.iter().find(|n| n.name == "httpx").unwrap().sha256,
            "d909fcccc110f8c7faf814ca82a9a4d816bc5a6dbfea25d6591d6985b8ba59ad"
        );
    }

    #[test]
    fn resolution_accepts_sdists_without_any_opt_in() {
        let args = pip_report_args("httpx==0.28.1", false);
        assert!(!args.iter().any(|a| a.contains("only-binary")), "{args:?}");
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(args.last(), Some(&"httpx==0.28.1"));
    }

    #[test]
    fn pip_child_sees_allowed_variables_and_no_credentials() {
        let vars = [
            ("PATH", std::env::var("PATH").unwrap_or_default()),
            ("HOME", "/home/u".to_string()),
            ("HTTPS_PROXY", "http://proxy.invalid:3128".to_string()),
            ("PIP_INDEX_URL", "https://idx.invalid/simple".to_string()),
            ("SSL_CERT_FILE", "/etc/ssl/ca.pem".to_string()),
            ("LANG", "C.UTF-8".to_string()),
            ("GITHUB_TOKEN", "fake-github-secret".to_string()),
            ("OSC_PASSWORD", "fake-osc-secret".to_string()),
            ("ANTHROPIC_API_KEY", "fake-anthropic-secret".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let mut cmd = pip_command(&["env"], vars);
        let out = cmd.stdin(Stdio::null()).output().expect("env must run");
        assert!(out.status.success());
        let seen = String::from_utf8_lossy(&out.stdout);
        let keys: Vec<&str> = seen
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .collect();

        for want in [
            "PATH",
            "HOME",
            "HTTPS_PROXY",
            "PIP_INDEX_URL",
            "SSL_CERT_FILE",
            "LANG",
        ] {
            assert!(keys.contains(&want), "{want} missing from {keys:?}");
        }
        assert!(!seen.contains("fake-github-secret"), "{seen}");
        assert!(!seen.contains("fake-osc-secret"), "{seen}");
        assert!(!seen.contains("fake-anthropic-secret"), "{seen}");
        for k in &keys {
            assert!(PIP_ENV_ALLOWLIST.contains(k), "{k} is not allowlisted");
        }
    }

    #[test]
    fn pip_inherits_only_allowlisted_environment() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
            ("OSC_PASSWORD", "x"),
            ("GITHUB_TOKEN", "x"),
            ("ANTHROPIC_API_KEY", "x"),
            ("PIP_INDEX_URL", "https://idx"),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let kept: Vec<String> = pip_env(vars)
            .into_iter()
            .map(|(k, _)| k.into_string().unwrap())
            .collect();
        assert_eq!(kept, ["PATH", "HOME", "PIP_INDEX_URL"]);
    }

    #[test]
    fn unknown_top_level_fields_do_not_break_parsing() {
        let json = HTTPX_REPORT.replace(
            r#""version": "1""#,
            r#""version": "1", "future_field": {"a": 1}"#,
        );
        assert_eq!(parse(&json).len(), 3);
    }

    #[test]
    fn cache_key_folds_interpreter_and_platform_into_a_short_hash() {
        let key = cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert!(key.starts_with("httpx-0.28.1-"), "{key}");
        assert!(key.ends_with(".json"), "{key}");
        // 12-char hex hash segment between the version and `.json`.
        let stem = key.strip_suffix(".json").unwrap();
        let hash = stem.rsplit('-').next().unwrap();
        assert_eq!(hash.len(), 12, "{key}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
    }

    #[test]
    fn cache_key_changes_when_interpreter_changes() {
        let a = cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        let b = cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.14.0",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_ne!(a, b, "interpreter must change the cache key");
    }

    #[test]
    fn cache_key_changes_when_platform_changes() {
        let a = cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        let b = cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-aarch64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_ne!(a, b, "platform must change the cache key");
    }

    #[test]
    fn cache_key_is_deterministic_for_the_same_inputs() {
        let a = cache_key(
            "Httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        let b = cache_key(
            "Httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_lowercases_the_package_name() {
        // Avoids `MarkupSafe` vs `markupsafe` producing two different cache
        // entries for the same release.
        let a = cache_key(
            "MarkupSafe",
            Some("3.0.3"),
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        let b = cache_key(
            "markupsafe",
            Some("3.0.3"),
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_distinguishes_latest_from_pinned() {
        let latest = cache_key(
            "httpx",
            None,
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        let pinned = cache_key(
            "httpx",
            Some("0.28.1"),
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_ne!(latest, pinned);
        assert!(latest.contains("httpx-latest-"), "{latest}");
    }

    #[test]
    fn cache_key_cannot_escape_the_cache_dir() {
        let key = cache_key(
            "../../etc/passwd",
            Some("1/../2"),
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert!(!key.contains('/'), "{key}");
        assert!(!key.contains(".."), "{key}");
        let a = cache_key(
            "Foo_Bar.baz",
            Some("1.0"),
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        let b = cache_key(
            "foo-bar-baz",
            Some("1.0"),
            "p",
            "l",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_eq!(a, b);
        assert!(a.starts_with("foo-bar-baz-1.0-"), "{a}");
    }

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn cache_key_changes_when_the_index_url_changes() {
        // Resolving against mirror A and then mirror B must not share an entry.
        let key = |idx: &str| {
            cache_key(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &env_of(&[("PIP_INDEX_URL", idx)]),
                None,
            )
        };
        let a = key("https://mirror-a.invalid/simple");
        let b = key("https://mirror-b.invalid/simple");
        assert_ne!(a, b, "PIP_INDEX_URL must change the cache key");
        assert_eq!(a, key("https://mirror-a.invalid/simple"));
    }

    #[test]
    fn every_resolution_variable_changes_the_cache_key() {
        let key = |vars: &BTreeMap<OsString, OsString>| {
            cache_key(
                "httpx",
                Some("0.28.1"),
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                vars,
                None,
            )
        };
        let unset = key(&BTreeMap::new());
        for var in RESOLUTION_ENV {
            let set = key(&env_of(&[(*var, "https://example.invalid/x")]));
            assert_ne!(unset, set, "{var} must change the cache key");
        }
    }

    #[test]
    fn unset_resolution_variables_give_a_stable_key() {
        let key = || {
            cache_key(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &BTreeMap::new(),
                None,
            )
        };
        assert_eq!(key(), key());
    }

    #[test]
    fn index_credentials_are_hashed_not_printed() {
        let key = cache_key(
            "httpx",
            None,
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &env_of(&[("PIP_INDEX_URL", "https://user:hunter2@idx.invalid/simple")]),
            None,
        );
        assert!(!key.contains("hunter2"), "{key}");
        assert!(!key.contains("idx.invalid"), "{key}");
    }

    #[test]
    fn the_content_of_the_pip_config_file_changes_the_cache_key() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("private-pip.conf");
        let conf_path = conf.to_str().unwrap();
        let key = |body: &str| {
            std::fs::write(&conf, body).unwrap();
            cache_key(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &env_of(&[("PIP_CONFIG_FILE", conf_path)]),
                None,
            )
        };
        let a = key("[global]\nindex-url = https://a.invalid/simple\n");
        let b = key("[global]\nindex-url = https://b.invalid/simple\n");
        assert_ne!(a, b, "config content must change the cache key");
        assert_eq!(a, key("[global]\nindex-url = https://a.invalid/simple\n"));
    }

    #[test]
    fn the_user_pip_conf_under_home_changes_the_cache_key() {
        let home = tempfile::tempdir().unwrap();
        let home_str = home.path().to_str().unwrap();
        let conf_dir = home.path().join(".config/pip");
        std::fs::create_dir_all(&conf_dir).unwrap();
        let key = |body: &str| {
            std::fs::write(conf_dir.join("pip.conf"), body).unwrap();
            cache_key(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &env_of(&[("HOME", home_str)]),
                None,
            )
        };
        let a = key("[global]\nextra-index-url = https://a.invalid/simple\n");
        let b = key("[global]\nextra-index-url = https://b.invalid/simple\n");
        assert_ne!(a, b, "~/.config/pip/pip.conf must change the cache key");
    }

    #[test]
    fn system_config_content_changes_the_cache_key() {
        // `/etc/pip.conf` itself cannot be written by a test, so the same
        // digest path is exercised with an injected candidate list.
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("etc-pip.conf");
        let key = |body: &str| {
            std::fs::write(&conf, body).unwrap();
            cache_key_with_configs(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &BTreeMap::new(),
                &[Some(conf.clone())],
            )
        };
        let a = key("[global]\nindex-url = https://a.invalid/simple\n");
        let b = key("[global]\nindex-url = https://b.invalid/simple\n");
        assert_ne!(a, b, "a system config's content must change the cache key");
        assert_eq!(a, key("[global]\nindex-url = https://a.invalid/simple\n"));
    }

    #[test]
    fn missing_config_files_give_a_stable_key() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such.conf");
        let key = |configs: &[Option<std::path::PathBuf>]| {
            cache_key_with_configs(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &BTreeMap::new(),
                configs,
            )
        };
        let absent = key(&[None, Some(missing.clone()), None]);
        assert_eq!(absent, key(&[None, Some(missing.clone()), None]));
        // A directory is not a config file either.
        assert_eq!(absent, key(&[None, Some(dir.path().to_path_buf()), None]));
        // Once the file exists, the key changes.
        std::fs::write(&missing, "[global]\n").unwrap();
        assert_ne!(absent, key(&[None, Some(missing.clone()), None]));
    }

    #[test]
    fn config_candidates_are_in_a_fixed_order() {
        let vars: BTreeMap<OsString, OsString> = [
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", "/x"),
            ("PIP_CONFIG_FILE", "/p.conf"),
            ("XDG_CONFIG_DIRS", "/a:rel:/b"),
        ]
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
        let got = config_candidates(&vars, Some(std::path::Path::new("/venv")));
        let p = |s: &str| Some(std::path::PathBuf::from(s));
        assert_eq!(
            got,
            vec![
                p("/p.conf"),
                p("/x/pip/pip.conf"),
                p("/h/.config/pip/pip.conf"),
                p("/h/.pip/pip.conf"),
                p("/etc/pip.conf"),
                p("/etc/xdg/pip/pip.conf"),
                p("/usr/local/etc/xdg/pip/pip.conf"),
                p("/usr/etc/xdg/pip/pip.conf"),
                p("/a/pip/pip.conf"),
                None, // relative XDG_CONFIG_DIRS entries are ignored
                p("/b/pip/pip.conf"),
                p("/venv/pip.conf"),
            ]
        );
    }

    #[test]
    fn unset_xdg_config_dirs_default_to_etc_xdg() {
        let got = config_candidates(&BTreeMap::new(), None);
        assert_eq!(
            got[8],
            Some(std::path::PathBuf::from("/etc/xdg/pip/pip.conf"))
        );
        assert_eq!(
            got.last(),
            Some(&None),
            "no prefix, so no <prefix>/pip.conf"
        );
        assert_eq!(got.len(), 10, "{got:?}");
    }

    #[test]
    fn the_interpreter_prefix_pip_conf_changes_the_cache_key() {
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path();
        let key = |body: Option<&str>| {
            if let Some(body) = body {
                std::fs::write(venv.join("pip.conf"), body).unwrap();
            }
            cache_key(
                "httpx",
                None,
                "Python 3.13.1",
                "linux-x86_64",
                ResolveMode::Sandboxed,
                &BTreeMap::new(),
                Some(venv),
            )
        };
        let without = key(None);
        let with = key(Some("[global]\nindex-url = https://a.invalid/simple\n"));
        assert_ne!(without, with, "<prefix>/pip.conf must change the cache key");
    }

    #[test]
    fn interpreter_prefix_is_found_on_path_without_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("venv/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("pip"), "").unwrap();
        let path = bin.clone().into_os_string();
        assert_eq!(
            interpreter_prefix_of("pip", &path),
            Some(dir.path().join("venv"))
        );
        assert_eq!(interpreter_prefix_of("missing-pip", &path), None);
        assert_eq!(
            interpreter_prefix_of("/opt/py/bin/python3", OsStr::new("")),
            Some(std::path::PathBuf::from("/opt/py"))
        );
        assert_eq!(
            interpreter_prefix_of("/opt/tool/pip", OsStr::new("")),
            Some(std::path::PathBuf::from("/opt/tool"))
        );
    }

    #[test]
    fn pip_version_names_the_interpreter_it_runs_under() {
        for (out, want) in [
            (
                "pip 24.0 from /usr/lib/python3/dist-packages/pip (python 3.12)\n",
                "Python 3.12",
            ),
            (
                "pip 23.3.1 from /home/u/.local/lib/python3.11/site-packages/pip (python 3.11)\n",
                "Python 3.11",
            ),
            (
                "pip 26.2.1 from /usr/lib/python3.13/site-packages/pip (python 3.13)\n",
                "Python 3.13",
            ),
            (
                "pip 25.1 from /opt/py/lib/python3.10/site-packages/pip (python 3.10.12)\n",
                "Python 3.10",
            ),
        ] {
            assert_eq!(parse_pip_version(out).as_deref(), Some(want), "{out}");
        }
    }

    #[test]
    fn pip_version_without_a_python_version_is_not_parsed() {
        assert_eq!(parse_pip_version(""), None);
        assert_eq!(parse_pip_version("pip 24.0 from /usr/lib/pip\n"), None);
        assert_eq!(parse_pip_version("pip 24.0 from /x (python 3)\n"), None);
        assert_eq!(parse_pip_version("pip 24.0 from /x (python x.y)\n"), None);
    }

    #[test]
    fn run_with_timeout_kills_a_slow_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let t = Instant::now();
        let err = run_with_timeout(cmd, Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, Error::Failed { .. }), "{err}");
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_with_timeout_collects_output() {
        let mut cmd = Command::new("echo");
        cmd.arg("hi");
        let out = run_with_timeout(cmd, Duration::from_secs(10)).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    #[test]
    fn run_with_timeout_does_not_wait_for_a_leftover_grandchild() {
        // The shell exits at once but its background `sleep` keeps the pipes
        // open; the group kill after exit must let the readers finish.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & echo hi"]);
        let t = Instant::now();
        let out = run_with_timeout(cmd, Duration::from_secs(10)).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_with_timeout_kills_grandchildren_holding_the_pipes() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let t = Instant::now();
        let err = run_with_timeout(cmd, Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, Error::Failed { .. }), "{err}");
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn cache_key_differs_from_the_old_narrow_scheme() {
        // Old key was `<name>-<version>.json`. The new key must never match it,
        // so stale entries from before this change are simply ignored (the TTL
        // in load_cached_resolve prunes them).
        let old = "httpx-0.28.1.json";
        let new = cache_key(
            "httpx",
            Some("0.28.1"),
            "Python 3.13.1",
            "linux-x86_64",
            ResolveMode::Sandboxed,
            &BTreeMap::new(),
            None,
        );
        assert_ne!(old, new);
    }

    #[test]
    fn resolve_modes_do_not_share_a_cache_entry() {
        // A wheels-only closure lacks sdist-only releases, and a closure
        // resolved with sdists may hold releases a wheels-only run must
        // reject: each mode gets its own entry.
        let key = |mode| {
            cache_key(
                "httpx",
                Some("0.28.1"),
                "Python 3.13.1",
                "linux-x86_64",
                mode,
                &BTreeMap::new(),
                None,
            )
        };
        let sandboxed = key(ResolveMode::Sandboxed);
        let wheels = key(ResolveMode::WheelsOnly);
        let unsandboxed = key(ResolveMode::Unsandboxed);
        assert_ne!(sandboxed, wheels);
        assert_ne!(sandboxed, unsandboxed);
        assert_ne!(wheels, unsandboxed);
    }

    #[test]
    fn an_entry_recorded_for_312_is_not_served_to_a_313_host() {
        // A closure resolved by `python3.12 -m pip` (the host's default was
        // incompatible) is stored under 3.12's key. A later run on a 3.13 host
        // must miss and resolve again, not get a closure from another Python.
        let dir = tempfile::tempdir().unwrap();
        let nodes = vec![Node {
            name: "httpx".into(),
            version: "0.28.1".into(),
            sha256: "ab".into(),
            requires_dist: vec![],
            requires_python: None,
            is_requested: true,
            is_yanked: false,
        }];
        let mode = ResolveMode::Sandboxed;
        save_cached_resolve(
            dir.path(),
            "httpx",
            Some("0.28.1"),
            "Python 3.12.7",
            None,
            mode,
            &nodes,
        );
        assert_eq!(
            load_cached_resolve(
                dir.path(),
                "httpx",
                Some("0.28.1"),
                "Python 3.12.7",
                None,
                mode
            ),
            Some(nodes)
        );
        assert_eq!(
            load_cached_resolve(
                dir.path(),
                "httpx",
                Some("0.28.1"),
                "Python 3.13.1",
                None,
                mode
            ),
            None
        );
    }
}

#[cfg(test)]
mod candidate_tests {
    use super::*;
    use std::io;

    const PREFIX: &str = "/home/u/.local";

    fn unsafe_prefix() -> Error {
        Error::Sandbox(SandboxError::UnsafePrefix {
            program: "pip".into(),
            prefix: PREFIX.into(),
            why: "it is inside a configuration or cache directory and is not a virtualenv or Python installation",
        })
    }

    fn missing() -> Error {
        Error::Launch(io::Error::new(io::ErrorKind::NotFound, "not on PATH"))
    }

    fn alternates() -> Vec<String> {
        vec!["python3.12".into(), "python3.11".into()]
    }

    #[test]
    fn unsafe_default_prefix_moves_on_to_the_next_interpreter() {
        // ~/.local/bin/pip is refused, but the alternate that works is used.
        let mut tried = Vec::new();
        let (alternate, out) = select_candidate(Err(unsafe_prefix()), &alternates(), |py| {
            tried.push(py.to_string());
            if py == "python3.12" {
                Ok(b"report".to_vec())
            } else {
                Err(missing())
            }
        })
        .unwrap();
        assert_eq!(alternate.as_deref(), Some("python3.12"));
        assert_eq!(out, b"report");
        assert_eq!(tried, ["python3.12"]);
    }

    #[test]
    fn unsafe_prefix_on_every_candidate_names_the_prefix() {
        let err = select_candidate(
            Err(unsafe_prefix()),
            &alternates(),
            |_| Err(unsafe_prefix()),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Sandbox(SandboxError::UnsafePrefix { .. })),
            "{err}"
        );
        assert!(err.to_string().contains(PREFIX), "{err}");
    }

    #[test]
    fn incompatible_python_still_moves_on() {
        let incompatible = || Error::IncompatiblePython {
            name: "open-webui".into(),
            example: "0.8.12".into(),
        };
        let (alternate, _) = select_candidate(Err(incompatible()), &alternates(), |py| {
            if py == "python3.11" {
                Ok(Vec::new())
            } else {
                Err(incompatible())
            }
        })
        .unwrap();
        assert_eq!(alternate.as_deref(), Some("python3.11"));
    }

    #[test]
    fn other_default_failures_are_final() {
        let mut tried = false;
        let err = select_candidate(Err(Error::NotFound("nope".into())), &alternates(), |_| {
            tried = true;
            Ok(Vec::new())
        })
        .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err}");
        assert!(
            !tried,
            "no alternate is tried after a final default failure"
        );
    }
}

#[cfg(test)]
mod classify_failure_tests {
    use super::*;

    #[test]
    fn skipped_for_requires_python_is_not_reported_as_missing() {
        let stderr = "ERROR: Ignored the following versions that require a different python version: 0.8.12 Requires-Python >=3.11,<3.13.0a1; 0.9.0 Requires-Python >=3.11,<3.13.0a1\nERROR: Could not find a version that satisfies the requirement open-webui (from versions: none)\nERROR: No matching distribution found for open-webui\n";
        match classify_failure("open-webui", 1, stderr.to_string(), false) {
            Error::IncompatiblePython { name, example } => {
                assert_eq!(name, "open-webui");
                assert_eq!(example, "0.8.12 Requires-Python >=3.11,<3.13.0a1");
            }
            e => panic!("wrong error: {e}"),
        }
    }

    #[test]
    fn a_truly_missing_project_is_not_found() {
        let stderr = "ERROR: Could not find a version that satisfies the requirement nope (from versions: none)\nERROR: No matching distribution found for nope\n";
        assert!(matches!(
            classify_failure("nope", 1, stderr.to_string(), false),
            Error::NotFound(_)
        ));
    }
}

#[cfg(test)]
mod sandbox_tests;
