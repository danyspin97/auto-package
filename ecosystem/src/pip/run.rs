use super::*;

/// Interpreters tried, in order, when the default pip's Python is outside a
/// project's `Requires-Python`. Stable releases first, newest-compatible
/// guesses before older ones.
pub(super) const ALTERNATE_PYTHON_MINORS: [u32; 5] = [12, 11, 13, 10, 14];

/// Run `pip install --dry-run --report -` through `program` (a pip command
/// line prefix) and return its JSON report.
pub(super) fn run_pip_report(
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

/// `program` run through bubblewrap, with a cleared environment for bwrap
/// itself and the sandbox's own environment built from the allowlisted
/// variables found in `vars`.
pub(super) fn sandboxed_command(
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
pub(super) const PIP_ENV_ALLOWLIST: &[&str] = &[
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
pub(super) fn pip_command(
    program: &[&str],
    vars: impl Iterator<Item = (OsString, OsString)>,
) -> Command {
    let mut cmd = Command::new(program[0]);
    cmd.env_clear().envs(pip_env(vars)).args(&program[1..]);
    cmd
}

/// The `pip install` arguments for one resolution. Sdists are not excluded
/// unless `wheels_only`, so pip can resolve a release that ships only an
/// sdist.
pub(super) fn pip_report_args(spec: &str, wheels_only: bool) -> Vec<&str> {
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
pub(super) fn pip_env(
    vars: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
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
pub(super) fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Result<std::process::Output, Error> {
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
