use super::*;

/// The `bwrap` arguments for running `program` (a pip command line prefix),
/// up to and including the program itself. The caller appends pip's own
/// arguments.
pub fn bwrap_args(program: &[&str], host: &Host) -> Result<Vec<OsString>, SandboxError> {
    let mut a = Args::default();
    a.flags([
        "--unshare-all",
        "--share-net",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--cap-drop",
        "ALL",
    ]);
    a.flags(ap_core::sandbox::system_ro_binds());
    a.flags(ap_core::sandbox::etc_ro_binds());
    a.flags(["--proc", "/proc", "--dev", "/dev"]);
    a.flags(["--tmpfs", "/tmp", "--tmpfs", "/run"]);
    a.flags(["--tmpfs", SANDBOX_HOME]);
    a.flags(["--dir", "/home/sandbox/.cache"]);

    let (exe, bin_dirs) = bind_interpreter(&mut a, program[0], host)?;
    bind_pip_config(&mut a, host);

    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for (k, v) in &host.vars {
        let Some(key) = k.to_str() else { continue };
        if OWN_VARS.contains(&key) {
            continue;
        }
        if CA_FILE_VARS.contains(&key) || key == "SSL_CERT_DIR" {
            // The variable is passed on with only the entries that the
            // sandbox actually has; when none are usable it is not set.
            if let Some(usable) = bind_ca(&mut a, key, v, host)? {
                env.push((k.clone(), usable));
            }
            continue;
        }
        env.push((k.clone(), v.clone()));
    }
    if let Some(cfg) = pip_config_env(&mut a, host) {
        env.push(("PIP_CONFIG_FILE".into(), cfg));
    }
    // Nothing but the tmpfs mounts above is writable: the root itself is a
    // tmpfs that would otherwise accept files (lost on exit, but writable).
    a.flags(["--remount-ro", "/"]);
    let mut path = OsString::new();
    for dir in bin_dirs {
        path.push(dir);
        path.push(":");
    }
    path.push("/usr/local/bin:/usr/bin:/bin");
    env.push(("PATH".into(), path));
    env.push(("HOME".into(), SANDBOX_HOME.into()));
    env.push(("TMPDIR".into(), "/tmp".into()));
    env.push(("XDG_CACHE_HOME".into(), "/home/sandbox/.cache".into()));
    env.push(("XDG_CONFIG_HOME".into(), "/home/sandbox/.config".into()));
    for (k, v) in env {
        a.0.push("--setenv".into());
        a.0.push(k);
        a.0.push(v);
    }

    a.flags(["--chdir", "/tmp", "--"]);
    a.0.push(exe.into_os_string());
    a.0.extend(program[1..].iter().map(OsString::from));
    Ok(a.0)
}

#[derive(Default)]
pub(super) struct Args(Vec<OsString>);

impl Args {
    fn flags<S: AsRef<OsStr>>(&mut self, flags: impl IntoIterator<Item = S>) {
        self.0
            .extend(flags.into_iter().map(|f| f.as_ref().to_owned()));
    }

    pub(super) fn ro_bind(&mut self, src: &Path, dst: &Path) {
        self.0.push("--ro-bind".into());
        self.0.push(src.into());
        self.0.push(dst.into());
    }
}

/// Bind the user's pip config files, each at the place pip looks for it in
/// the sandbox home.
pub(super) fn bind_pip_config(a: &mut Args, host: &Host) {
    let xdg = host
        .vars
        .iter()
        .find(|(k, _)| k == "XDG_CONFIG_HOME")
        .map(|(_, v)| PathBuf::from(v))
        .filter(|p| p.is_absolute());
    let mut candidates = vec![
        (
            host.home.join(".config/pip/pip.conf"),
            "/home/sandbox/.config/pip/pip.conf",
        ),
        (
            host.home.join(".pip/pip.conf"),
            "/home/sandbox/.pip/pip.conf",
        ),
    ];
    if let Some(xdg) = xdg {
        // pip reads `$XDG_CONFIG_HOME/pip/pip.conf` as well; in the sandbox
        // `XDG_CONFIG_HOME` is `~/.config`, so it lands in the same place.
        candidates.insert(
            0,
            (
                xdg.join("pip/pip.conf"),
                "/home/sandbox/.config/pip/pip.conf",
            ),
        );
    }
    let mut seen_dst = BTreeSet::new();
    for (src, dst) in candidates {
        if src.is_file() && seen_dst.insert(dst) {
            if let Some(why) = sensitive_file(&src, &host.home) {
                tracing::warn!("skipping pip config {}: {why}", src.display());
                continue;
            }
            a.ro_bind(&src, Path::new(dst));
        }
    }
    if host.netrc {
        let netrc = host.home.join(".netrc");
        if netrc.is_file() {
            a.ro_bind(&netrc, Path::new("/home/sandbox/.netrc"));
        }
    }
}

/// The sandbox's `PIP_CONFIG_FILE`, binding the host's file when it names one.
pub(super) fn pip_config_env(a: &mut Args, host: &Host) -> Option<OsString> {
    let (_, value) = host.vars.iter().find(|(k, _)| k == "PIP_CONFIG_FILE")?;
    // `os.devnull` is how pip is told to read no config at all.
    if value == "/dev/null" {
        return Some(value.clone());
    }
    let src = PathBuf::from(value);
    if !src.is_absolute() || !src.is_file() {
        return None;
    }
    if let Some(why) = sensitive_file(&src, &host.home) {
        tracing::warn!("skipping PIP_CONFIG_FILE {}: {why}", src.display());
        return None;
    }
    a.ro_bind(&src, Path::new(SANDBOX_PIP_CONFIG_FILE));
    Some(SANDBOX_PIP_CONFIG_FILE.into())
}

/// Make the CA bundle files or directories named by `var` usable in the
/// sandbox. `SSL_CERT_DIR` is a colon-separated list and each entry is handled
/// as a single path; the others name one path.
///
/// An entry is bound at its own path when it exists and is not already in the
/// sandbox. An entry that is relative, missing or unusable is skipped with a
/// warning. An entry that names credentials is refused with an error, as
/// before.
///
/// Returns the value to pass into the sandbox: the usable entries joined the
/// same way, or `None` when no entry is usable and the variable should not be
/// set at all.
pub(super) fn bind_ca(
    a: &mut Args,
    var: &str,
    value: &OsString,
    host: &Host,
) -> Result<Option<OsString>, SandboxError> {
    let raw = value.as_bytes();
    let sep = if var == "SSL_CERT_DIR" {
        Some(b':')
    } else {
        None
    };
    let entries: Vec<&[u8]> = match sep {
        Some(c) => raw.split(|b| *b == c).collect(),
        None => vec![raw],
    };
    let mut usable: Vec<Vec<u8>> = Vec::new();
    for entry in entries {
        if entry.is_empty() {
            continue;
        }
        let path = PathBuf::from(OsStr::from_bytes(entry));
        if !path.is_absolute() {
            tracing::warn!(
                "{var} entry {} is not an absolute path; not exposing it to the sandbox",
                path.display()
            );
            continue;
        }
        if !path.exists() {
            tracing::warn!(
                "{var} entry {} does not exist; not exposing it to the sandbox",
                path.display()
            );
            continue;
        }
        // `..` is resolved before anything is decided: `/usr/../home/u/.ssh/key`
        // is not under `/usr`. A path that climbs above `/` is not exposed.
        let Some(normal) = normalize_lexically(&path) else {
            tracing::warn!(
                "{var} entry {} climbs above the root; not exposing it to the sandbox",
                path.display()
            );
            continue;
        };
        let Ok(resolved) = path.canonicalize() else {
            tracing::warn!(
                "{var} entry {} cannot be resolved; not exposing it to the sandbox",
                path.display()
            );
            continue;
        };
        // Both the path as named and what it resolves to through symlinks and
        // `..` must be free of credentials. `sensitive_file` also resolves the
        // normalized path, which is what gets bound.
        if let Some(why) =
            sensitive_reason(&resolved, &host.home).or_else(|| sensitive_file(&normal, &host.home))
        {
            return Err(SandboxError::UnsafePath {
                var: var.to_string(),
                path,
                why,
            });
        }
        if is_under_any(&normal, SYSTEM_DIRS) || is_under_any(&normal, ETC_ENTRIES) {
            // Already present in the sandbox at this path.
            usable.push(normal.as_os_str().as_bytes().to_vec());
            continue;
        }
        a.ro_bind(&normal, &normal);
        usable.push(normal.as_os_str().as_bytes().to_vec());
    }
    if usable.is_empty() {
        return Ok(None);
    }
    let joined: Vec<u8> = usable.join(&b':');
    Ok(Some(OsStr::from_bytes(&joined).to_owned()))
}
