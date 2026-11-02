use super::*;

/// What one sandboxed run binds and sets.
pub(crate) struct Profile<'a> {
    /// The checkout, absolute and canonical: bound read-write at its path.
    pub checkout: &'a Path,
    /// Caches and temporary files, inside the checkout.
    pub scratch: &'a Path,
    /// The proxy's socket directory on the host.
    pub proxy_dir: &'a Path,
    /// More variables (the changelog identity).
    pub extra_env: &'a [(String, String)],
    /// The hosts file bound over the sandbox's `/etc/hosts`.
    pub hosts_file: &'a Path,
    /// The address each mapped host name is reached at, as in `hosts_file`
    /// (see `bridge_addresses`): the bridge listens on port 443 for them.
    pub tunnels: &'a [(String, String)],
}

pub(super) fn utf8(p: &Path) -> Result<&str, String> {
    p.to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))
}

/// The environment inside the sandbox: nothing is inherited.
pub(crate) fn sandbox_env(p: &Profile<'_>) -> Result<Vec<(String, String)>, String> {
    let scratch = utf8(p.scratch)?;
    let proxy = format!("http://127.0.0.1:{PROXY_PORT}");
    let mut env: Vec<(String, String)> = [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
        ("LANG", "C.UTF-8".to_string()),
        ("HOME", SANDBOX_HOME.to_string()),
        ("TMPDIR", format!("{scratch}/tmp")),
        ("XDG_CACHE_HOME", format!("{scratch}/cache")),
        ("CARGO_HOME", format!("{scratch}/cargo")),
        ("npm_config_cache", format!("{scratch}/npm")),
        ("GOPATH", format!("{scratch}/go")),
        ("GOMODCACHE", format!("{scratch}/go/pkg/mod")),
        // The module cache is read-only otherwise, and could not be removed.
        ("GOFLAGS", "-modcacherw".to_string()),
        ("OSC_VERSION", OSC_VERSION.to_string()),
        ("http_proxy", proxy.clone()),
        ("https_proxy", proxy.clone()),
        ("HTTP_PROXY", proxy.clone()),
        ("HTTPS_PROXY", proxy.clone()),
        ("no_proxy", String::new()),
        ("NO_PROXY", String::new()),
        ("CARGO_HTTP_PROXY", proxy.clone()),
        ("npm_config_proxy", proxy.clone()),
        ("npm_config_https_proxy", proxy.clone()),
        ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".to_string()),
        ("GIT_CONFIG_COUNT", "2".to_string()),
        ("GIT_CONFIG_KEY_0", "safe.directory".to_string()),
        ("GIT_CONFIG_VALUE_0", "*".to_string()),
        ("GIT_CONFIG_KEY_1", "http.proxy".to_string()),
        ("GIT_CONFIG_VALUE_1", proxy),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    env.extend(p.extra_env.iter().cloned());
    Ok(env)
}

/// `/dev` for the sandbox: the device nodes bubblewrap's `--dev` makes, bound
/// from the host, and no pseudo-terminals. `--dev` itself makes a privileged
/// bind (the TLS bridge's port 443) fail even with `CAP_NET_BIND_SERVICE`.
pub(super) fn dev_args() -> Vec<String> {
    let mut args = vec!["--tmpfs".to_string(), "/dev".to_string()];
    for node in ["null", "zero", "full", "random", "urandom", "tty"] {
        let path = format!("/dev/{node}");
        args.extend(["--dev-bind-try".to_string(), path.clone(), path]);
    }
    for (src, dest) in [
        ("/proc/self/fd", "/dev/fd"),
        ("/proc/self/fd/0", "/dev/stdin"),
        ("/proc/self/fd/1", "/dev/stdout"),
        ("/proc/self/fd/2", "/dev/stderr"),
    ] {
        args.extend(["--symlink".to_string(), src.into(), dest.into()]);
    }
    args.extend(["--tmpfs".to_string(), "/dev/shm".to_string()]);
    args
}

/// The `bwrap` arguments that run `program` in the sandbox `p` describes.
pub(crate) fn bwrap_args(p: &Profile<'_>, program: &[String]) -> Result<Vec<String>, String> {
    let checkout = utf8(p.checkout)?;
    if !p.checkout.is_absolute() {
        return Err(format!("{checkout} is not absolute"));
    }
    let osc = format!("{checkout}/.osc");
    let proxy_dir = utf8(p.proxy_dir)?;
    let mut args: Vec<String> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        // The bridge binds port 443 in the sandbox's network namespace. The
        // services start without it: see the `setpriv` below.
        "--cap-add",
        "CAP_NET_BIND_SERVICE",
    ]
    .map(String::from)
    .into();
    args.extend(ap_core::sandbox::system_ro_binds());
    args.extend(ap_core::sandbox::etc_ro_binds());
    args.extend(
        [
            // tar_scm's site configuration.
            "--ro-bind-try",
            "/etc/obs",
            "/etc/obs",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
            "--dir",
            SANDBOX_HOME,
            "--bind",
            checkout,
            checkout,
            // osc commits to the API named in `.osc/_apiurl`, with the user's
            // credentials: a service must not be able to change it.
            "--ro-bind-try",
            &osc,
            &osc,
            "--ro-bind",
            proxy_dir,
            SANDBOX_PROXY_DIR,
            "--chdir",
            checkout,
            "--clearenv",
        ]
        .map(String::from),
    );
    args.extend(dev_args());
    let hosts = utf8(p.hosts_file)?;
    args.extend(["--ro-bind", hosts, "/etc/hosts"].map(String::from));
    for (k, v) in sandbox_env(p)? {
        args.extend(["--setenv".to_string(), k, v]);
    }
    args.extend(
        [
            "--",
            PYTHON,
            "-I",
            "-S",
            "-c",
            BRIDGE,
            &format!("{SANDBOX_PROXY_DIR}/{SOCKET_NAME}"),
            &PROXY_PORT.to_string(),
        ]
        .map(String::from),
    );
    args.extend(p.tunnels.iter().map(|(ip, name)| format!("{ip}={name}")));
    args.push("--".to_string());
    // A service starts with no capabilities, whatever the bridge holds.
    args.extend(
        [
            SETPRIV,
            "--inh-caps=-all",
            "--ambient-caps=-all",
            "--no-new-privs",
            "--",
        ]
        .map(String::from),
    );
    args.extend(program.iter().cloned());
    Ok(args)
}

/// One `<service>` of a validated `_service`, its params decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceCall {
    pub name: String,
    pub params: Vec<(String, String)>,
}

/// The helper command line osc builds for `s`.
pub(crate) fn service_argv(s: &ServiceCall, outdir: &Path) -> Vec<String> {
    let mut argv = vec![format!("{SERVICE_DIR}/{}", s.name)];
    for (name, value) in &s.params {
        argv.push(format!("--{name}"));
        argv.push(value.clone());
    }
    argv.push("--outdir".to_string());
    argv.push(outdir.to_string_lossy().into_owned());
    argv
}

/// Whether services run unsandboxed on request; warns once when they do.
pub(crate) fn unsafe_mode() -> bool {
    let on = ap_core::env::env_flag(UNSAFE_ENV);
    if on {
        static WARNED: Once = Once::new();
        WARNED.call_once(|| {
            ap_core::diag!(
                "WARNING: {UNSAFE_ENV} is set: `osc service` runs WITHOUT a sandbox, with your \
                 home directory, the session bus (your OBS credentials) and the whole network \
                 reachable by the sources it fetches"
            );
        });
    }
    on
}

/// The `bwrap` to run, or why services cannot be sandboxed here.
pub(crate) fn sandbox_ready() -> Result<&'static Path, Error> {
    let why = match ap_core::sandbox::bwrap_path() {
        None => "bwrap is not installed".to_string(),
        Some(_) if !ap_core::sandbox::bwrap_works() => {
            "bwrap cannot create user namespaces here".to_string()
        }
        Some(_) if !Path::new(PYTHON).is_file() => format!("{PYTHON} is missing"),
        // Without it a service would keep the bridge's capabilities.
        Some(_) if !Path::new(SETPRIV).is_file() => {
            format!("{SETPRIV} is missing (install util-linux)")
        }
        Some(bwrap) => return Ok(bwrap),
    };
    Err(Error::ServiceSandbox(format!(
        "{why}. Install bubblewrap and allow unprivileged user namespaces \
         (sysctl user.max_user_namespaces=15000; kernel.unprivileged_userns_clone=1 where it \
         exists; kernel.apparmor_restrict_unprivileged_userns=0 on AppArmor systems), or set \
         {UNSAFE_ENV}=1 to run services unsandboxed"
    )))
}

/// The changelog identity tar_scm's `changesgenerate` needs, as osc would
/// export it: `VC_REALNAME`/`VC_MAILADDR` from the environment, else
/// `realname`/`email` of the checkout's API in the oscrc. Nothing else of the
/// oscrc is read.
pub(super) fn changes_identity(checkout: &Path) -> Vec<(String, String)> {
    let from_env: Vec<(String, String)> = ["VC_REALNAME", "VC_MAILADDR"]
        .iter()
        .filter_map(|k| {
            let v = std::env::var(k).ok().filter(|v| !v.trim().is_empty())?;
            Some((k.to_string(), v))
        })
        .collect();
    if !from_env.is_empty() {
        return from_env;
    }
    let Ok(apiurl) = fs::read_to_string(checkout.join(".osc").join("_apiurl")) else {
        return Vec::new();
    };
    oscrc_paths()
        .into_iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .map(|text| oscrc_identity(&text, apiurl.trim()))
        .unwrap_or_default()
}

pub(super) fn oscrc_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = std::env::var_os("OSC_CONFIG") {
        paths.push(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    if let Some(c) = config {
        paths.push(c.join("osc").join("oscrc"));
    }
    if let Some(h) = home {
        paths.push(h.join(".oscrc"));
    }
    paths
}

/// `realname` and `email` from the oscrc section of `apiurl`.
pub(super) fn oscrc_identity(text: &str, apiurl: &str) -> Vec<(String, String)> {
    fn norm(url: &str) -> String {
        let url = url.trim().to_ascii_lowercase();
        let url = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(&url);
        url.trim_end_matches('/').to_string()
    }
    let want = norm(apiurl);
    let mut in_section = false;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = norm(name) == want;
            continue;
        }
        if !in_section || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some((key, value)) = line.split_once(['=', ':']) {
            let var = match key.trim() {
                "realname" => "VC_REALNAME",
                "email" => "VC_MAILADDR",
                _ => continue,
            };
            let value = value.trim();
            if !value.is_empty() {
                out.push((var.to_string(), value.to_string()));
            }
        }
    }
    out
}
