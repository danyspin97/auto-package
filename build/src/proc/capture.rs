use super::*;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("could not launch {program}: {source}")]
    Launch {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("{program} did not finish within {secs}s and was killed")]
    Timeout { program: String, secs: u64 },
    /// A stream could not be read to its end, so an empty or short capture
    /// would be indistinguishable from real output.
    #[error("output of {program} could not be read completely ({stream})")]
    OutputLost {
        program: String,
        stream: &'static str,
    },
}

/// A minimal environment for tools that need no credentials: search path,
/// home, locale, terminal, proxies, CA bundles, XDG dirs. Variables with one
/// of `extra_prefixes` (upper-case) are kept as well.
pub fn minimal_env(extra_prefixes: &[&str]) -> std::collections::BTreeMap<String, String> {
    const EXACT: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LANGUAGE",
        "TERM",
        "TZ",
        "TMPDIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
    ];
    ap_core::env::utf8_vars()
        .into_iter()
        .filter(|(name, _)| {
            let upper = name.to_ascii_uppercase();
            EXACT.contains(&upper.as_str())
                || upper.starts_with("LC_")
                || upper.starts_with("XDG_")
                || extra_prefixes.iter().any(|p| upper.starts_with(p))
        })
        .collect()
}

/// Where a subprocess reports the fact that it ran.
///
/// Carried explicitly rather than read from a global: the subprocess layer is
/// the one place where "what did this program actually do" is knowable, and
/// `--log debug` promises every argv and cwd.
#[derive(Clone, Copy)]
pub struct Emit<'a> {
    pub reporter: &'a dyn Reporter,
    /// The numbered step this ran under, when there is one. Resolution and
    /// other pre-step work has none.
    pub step: Option<StepId>,
}

impl<'a> Emit<'a> {
    pub fn new(reporter: &'a dyn Reporter, step: StepId) -> Self {
        Self {
            reporter,
            step: Some(step),
        }
    }

    /// Report against the run rather than a numbered step.
    pub fn run(reporter: &'a dyn Reporter) -> Self {
        Self {
            reporter,
            step: None,
        }
    }

    /// Record nothing. For calls whose individual invocations are noise —
    /// a status poll repeated every 15 seconds, say.
    pub fn quiet() -> Emit<'static> {
        Emit {
            reporter: &NULL_REPORTER,
            step: None,
        }
    }

    pub(super) fn started(&self, program: &str, argv: &[String], cwd: &Path, log: Option<&Path>) {
        self.reporter.report(&Event::Command {
            step: self.step,
            program: program.to_string(),
            argv: argv.to_vec(),
            cwd: cwd.to_path_buf(),
            log: log.map(Path::to_path_buf),
        });
    }

    pub(super) fn done(&self, code: i32, elapsed: Duration, stdout_bytes: u64, stderr_bytes: u64) {
        self.reporter.report(&Event::CommandDone {
            step: self.step,
            code,
            elapsed,
            stdout_bytes,
            stderr_bytes,
        });
    }
}

/// Wall-clock limit applied by [`capture`] when the caller has no tighter one:
/// generous enough for `osc service ra` or a large `osc ci`, finite so a
/// stalled connection cannot hang the run forever.
pub const DEFAULT_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Cap on what one captured stream keeps. A chatty child is still drained
/// (so it never blocks on a full pipe) but only the last this-many bytes are
/// kept: a failing build's cause is at the end of its log.
pub(super) const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

/// A directory only this process can use, for scratch files and as the working
/// directory of helpers that read configuration from the directory they start
/// in (`gh` and `git` look for `.git`, `osc` for `.osc`).
///
/// The shared temporary directory is writable by every local user, so a path
/// there can be pre-created by someone else (a predictable name, a planted
/// `.git`), and a symlink or a hostile file in it is read as if it were ours.
/// This one is created once, with a random name and mode 0700. If it cannot be
/// created, a per-user `0700` directory under the temp dir is used after
/// checking its owner and mode; if that fails too the error is returned and
/// the caller must fail, never fall back to a shared path.
pub fn private_dir() -> std::io::Result<&'static Path> {
    static DIR: std::sync::OnceLock<Result<PathBuf, (std::io::ErrorKind, String)>> =
        std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .prefix("auto-package-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map(tempfile::TempDir::keep)
            .or_else(|_| {
                // SAFETY: getuid has no preconditions and cannot fail.
                per_user_dir(&std::env::temp_dir(), unsafe { libc::getuid() })
            })
            .map_err(|e| (e.kind(), e.to_string()))
    })
    .as_ref()
    .map(PathBuf::as_path)
    .map_err(|(kind, msg)| {
        std::io::Error::new(*kind, format!("no private scratch directory: {msg}"))
    })
}

/// `<tmp>/auto-package-<uid>`, created with mode 0700 when missing and accepted
/// only if it is a real directory (not a link) owned by `uid` with no group or
/// other access.
pub(super) fn per_user_dir(tmp: &Path, uid: u32) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = tmp.join(format!("auto-package-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory of this user", dir.display()),
        ));
    }
    Ok(dir)
}

/// Remove [`private_dir`] and everything in it. Best effort, for process exit.
pub fn remove_private_dir() {
    // The interrupt handler and `main` can both get here as the process ends;
    // the second would race the first's `remove_dir_all`.
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    crate::sources::remove_scratch_dir();
    if let Ok(dir) = private_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Write command output to a log file: credentials removed by
/// [`ap_core::redact::redact`] (the same pass the run log's argv gets), the
/// file readable by this user only. An existing file is replaced and brought to
/// that mode too. Output that is not UTF-8 is written with U+FFFD in its place.
pub fn write_log(path: &Path, output: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let redacted = ap_core::redact::redact(&String::from_utf8_lossy(output));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(redacted.as_bytes())
}

/// Append `chunk` to `buf`, then drop from the front so at most `cap` bytes
/// remain — the tail of everything written so far.
pub(super) fn push_tail(buf: &mut VecDeque<u8>, chunk: &[u8], cap: usize) {
    buf.extend(chunk);
    if buf.len() > cap {
        buf.drain(..buf.len() - cap);
    }
}

/// Run a subprocess and capture stdout/stderr separately as strings.
///
/// Intended for short commands (`osc ls`, `rpmspec --parse`) where the caller
/// needs to inspect the output. Emits the same `Command`/`CommandDone` pair as
/// [`run`], so `--log debug` shows every subprocess and not just the build.
/// Bounded by [`DEFAULT_CAPTURE_TIMEOUT`]; use [`capture_with_timeout`] for a
/// different limit.
pub fn capture(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    emit: Emit<'_>,
) -> Result<Captured, ProcessError> {
    capture_with_timeout(program, args, cwd, emit, DEFAULT_CAPTURE_TIMEOUT)
}

/// [`capture`] with a wall-clock limit: the child is killed if it has not
/// exited after `timeout`.
///
/// For network-bound helpers polled in a loop (`osc api`, `osc results`): a
/// stalled connection otherwise blocks the poller forever, and with it the
/// loop's own overall deadline, which is only ever checked between polls.
pub fn capture_with_timeout(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    emit: Emit<'_>,
    timeout: Duration,
) -> Result<Captured, ProcessError> {
    capture_with_timeout_env(program, args, cwd, emit, timeout, None)
}

/// [`capture_with_timeout`] with an explicit environment: when `env` is
/// `Some`, the child starts from an empty environment holding exactly those
/// variables (used to run the agent CLI with credentials scrubbed).
pub fn capture_with_timeout_env(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    emit: Emit<'_>,
    timeout: Duration,
    env: Option<&std::collections::BTreeMap<String, String>>,
) -> Result<Captured, ProcessError> {
    use std::io::Read;

    let timeout = if restoring() {
        timeout.min(RESTORE_TIMEOUT)
    } else {
        timeout
    };
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    emit.started(program, &argv, cwd, None);

    // The one chokepoint every captured `osc` call (`osc api`, `osc results`,
    // `osc ls`...) passes through: one shared token bucket for the whole run.
    if Path::new(program).file_name().is_some_and(|n| n == "osc") {
        ap_core::limits::obs_acquire();
    }
    let start = Instant::now();
    let mut command = Command::new(program);
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    // Own process group, so a timeout can kill the child's descendants too:
    // grandchildren (the agent CLI's tool subprocesses) would otherwise keep
    // the output pipes open and block the reader threads below forever.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ProcessError::Launch {
            program: program.to_string(),
            source: e,
        })?;
    let group = match track_group(child.id()) {
        Ok(g) => g,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
    };

    // Drain both pipes on their own threads so a chatty child can't fill a
    // pipe buffer and deadlock against our `try_wait` polling. Each keeps at
    // most MAX_CAPTURE_BYTES, the tail of the stream, and reports back over a
    // channel so the join below can have a deadline.
    // The bool is true when a read failed, i.e. the bytes are incomplete.
    fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::sync::mpsc::Receiver<(Vec<u8>, bool)> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = VecDeque::new();
            let mut cut_short = false;
            if let Some(mut r) = r {
                let mut chunk = [0u8; 16 * 1024];
                let mut read_errors = 0u32;
                loop {
                    match r.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            read_errors = 0;
                            push_tail(&mut buf, &chunk[..n], MAX_CAPTURE_BYTES as usize);
                        }
                        // A signal is not the end of the stream.
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        // Keep draining: stopping would leave the child blocked
                        // on a full pipe. The bytes are still marked incomplete,
                        // and only a run of errors ends the drain.
                        Err(_) => {
                            cut_short = true;
                            read_errors += 1;
                            if read_errors >= 16 {
                                break;
                            }
                        }
                    }
                }
            }
            let _ = tx.send((Vec::from(buf), cut_short));
        });
        rx
    }
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());

    // The exit is noticed without reaping the child, and the group is swept
    // and unregistered before it is reaped: until then the child still owns
    // its pid, so the sweep cannot hit an unrelated group that was handed the
    // number, and neither can a shutdown sweep that still lists it.
    let mut group = Some(group);
    let status = loop {
        match ap_core::pgroup::has_exited(child.id()) {
            // A descendant it left behind can still hold the pipes open: reap
            // the group, then wait only briefly for the readers.
            Ok(true) => {
                kill_process_group(child.id());
                drop(group.take());
                match child.wait() {
                    Ok(status) => break Some(status),
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(false) if start.elapsed() >= timeout => {
                kill_process_group(child.id());
                drop(group.take());
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                // Don't leave the child (and its descendants) running.
                kill_process_group(child.id());
                drop(group.take());
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
    };
    // `None` when the reader never reported back or its read failed: empty
    // output then means "unknown", not "the child printed nothing".
    // One shared deadline: a second full wait would let a slow descendant
    // turn a stream that was unknown at 5s into a known one at 10s.
    let deadline = Instant::now() + Duration::from_secs(5);
    // Returns the stream when it was read to EOF, and whether a read failed.
    // A failed read is not the same as a reader that never answered (a
    // descendant holding the pipe): the former is always an error.
    let collect = |rx: std::sync::mpsc::Receiver<(Vec<u8>, bool)>| match rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        Ok((buf, false)) => (Some(buf), false),
        Ok((_, true)) => (None, true),
        _ => (None, false),
    };
    let (stdout, stdout_failed) = collect(out_rx);
    let (stderr, stderr_failed) = collect(err_rx);
    let elapsed = start.elapsed();
    let len = |o: &Option<Vec<u8>>| o.as_ref().map_or(0, |b| b.len() as u64);

    let code = status.map_or(-1, |s| s.code().unwrap_or(-1));
    emit.done(code, elapsed, len(&stdout), len(&stderr));

    if status.is_none() {
        return Err(ProcessError::Timeout {
            program: program.to_string(),
            secs: timeout.as_secs(),
        });
    }
    let lost = |stream| ProcessError::OutputLost {
        program: program.to_string(),
        stream,
    };
    if stdout_failed {
        return Err(lost("stdout"));
    }
    if stderr_failed {
        return Err(lost("stderr"));
    }
    let (stdout, stderr) = match (stdout.as_ref(), stderr.as_ref()) {
        (Some(o), Some(e)) => (o, e),
        // A clean exit with one stream read to EOF: the other pipe is held by
        // a descendant that escaped the group (another session), not lost
        // data from the child. Discarding the complete stream would be worse.
        (Some(o), None) if code == 0 => (o, &Vec::new()),
        (None, Some(e)) if code == 0 => (&Vec::new(), e),
        _ => return Err(lost(if stdout.is_none() { "stdout" } else { "stderr" })),
    };
    Ok(Captured {
        code,
        stdout: String::from_utf8_lossy(stdout).into_owned(),
        stderr: String::from_utf8_lossy(stderr).into_owned(),
    })
}

pub struct Captured {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}
