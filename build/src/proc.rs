//! Subprocess execution with single-fd log capture and event emission.
//!
//! Both stdout and stderr are redirected to the same file descriptor so
//! the combined log is strictly chronological.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ap_core::event::{Event, StepId};
use ap_core::sink::{NULL_REPORTER, Reporter};

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
}

/// Where a subprocess reports the fact that it ran.
///
/// Carried explicitly rather than read from a global: the subprocess layer is
/// the one place where "what did this program actually do" is knowable, and
/// `--log debug` promises every argv and cwd (REDESIGN §3.4).
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

    fn started(&self, program: &str, argv: &[String], cwd: &Path, log: Option<&Path>) {
        self.reporter.report(&Event::Command {
            step: self.step,
            program: program.to_string(),
            argv: argv.to_vec(),
            cwd: cwd.to_path_buf(),
            log: log.map(Path::to_path_buf),
        });
    }

    fn done(&self, code: i32, elapsed: Duration, stdout_bytes: u64, stderr_bytes: u64) {
        self.reporter.report(&Event::CommandDone {
            step: self.step,
            code,
            elapsed,
            stdout_bytes,
            stderr_bytes,
        });
    }
}

pub struct ProcessOutput {
    pub code: i32,
    pub log_path: PathBuf,
    /// Total bytes written (stdout+stderr merged).
    pub stdout_bytes: u64,
    /// Always 0 — stderr is merged into stdout_bytes.
    pub stderr_bytes: u64,
    pub elapsed: Duration,
    /// Combined stdout+stderr content (same bytes as log_path).
    pub combined: Vec<u8>,
}

impl ProcessOutput {
    pub fn combined_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.combined)
    }

    pub fn success(&self) -> bool {
        self.code == 0
    }
}

/// Limit for [`run`] (package builds): long enough for the biggest compile,
/// finite so a wedged child cannot hold the run forever.
const RUN_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);

/// The last `cap` bytes of `path` (all of it when smaller); empty on error.
fn read_tail(path: &Path, cap: u64) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > cap {
        let _ = f.seek(SeekFrom::Start(len - cap));
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    buf
}

/// Run a subprocess, capturing stdout+stderr to a single log file.
///
/// Both file descriptors share the same underlying file description so
/// writes are interleaved in arrival order. Emits `Event::Command` before
/// launch and `Event::CommandDone` after completion.
pub fn run(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    env: &HashMap<String, String>,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<ProcessOutput, ProcessError> {
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();

    emit.started(program, &argv, cwd, Some(log_path));

    // Both stdout and stderr go to the same file; try_clone shares the offset.
    let log_file = File::create(log_path)?;
    let log_clone = log_file.try_clone()?;

    let start = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .envs(env.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_clone))
        .stderr(Stdio::from(log_file));
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(|e| ProcessError::Launch {
        program: program.to_string(),
        source: e,
    })?;
    let _group = track_group(child.id());
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if start.elapsed() >= RUN_TIMEOUT => {
                kill_process_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                emit.done(-1, start.elapsed(), 0, 0);
                return Err(ProcessError::Timeout {
                    program: program.to_string(),
                    secs: RUN_TIMEOUT.as_secs(),
                });
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let elapsed = start.elapsed();

    let combined = read_tail(log_path, MAX_CAPTURE_BYTES);
    let total_bytes = combined.len() as u64;
    let code = status.code().unwrap_or(-1);

    emit.done(code, elapsed, total_bytes, 0);

    Ok(ProcessOutput {
        code,
        log_path: log_path.to_path_buf(),
        stdout_bytes: total_bytes,
        stderr_bytes: 0,
        elapsed,
        combined,
    })
}

/// Wall-clock limit applied by [`capture`] when the caller has no tighter one:
/// generous enough for `osc service ra` or a large `osc ci`, finite so a
/// stalled connection cannot hang the run forever.
pub const DEFAULT_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Cap on what one captured stream keeps. A chatty child is still drained
/// (so it never blocks on a full pipe) but the excess is discarded.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

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

/// SIGKILL a child's whole process group (the child must have been spawned
/// with `process_group(0)`). Best effort: the group may already be gone.
/// Process groups of children currently running. Children get their own group
/// (so a timeout can kill their descendants), which also means a terminal
/// Ctrl-C no longer reaches them — when this process is told to stop it must
/// kill them itself or `osc`/`claude` keep running, orphaned.
static ACTIVE_GROUPS: std::sync::Mutex<std::collections::BTreeSet<u32>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Registration of a running child's group; removed on drop.
pub(crate) struct GroupGuard(u32);

/// Record `pid` (spawned with `process_group(0)`) as running until the guard drops.
pub(crate) fn track_group(pid: u32) -> GroupGuard {
    ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(pid);
    GroupGuard(pid)
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        ACTIVE_GROUPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// SIGKILL every child still running. For process shutdown (Ctrl-C, quit).
pub fn kill_all_groups() {
    let pids: Vec<u32> = ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    for pid in pids {
        kill_process_group(pid);
    }
}

#[cfg(unix)]
pub(crate) fn kill_process_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
pub(crate) fn kill_process_group(_pid: u32) {}

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

    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    emit.started(program, &argv, cwd, None);

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
    let _group = track_group(child.id());

    // Drain both pipes on their own threads so a chatty child can't fill a
    // pipe buffer and deadlock against our `try_wait` polling. Each keeps at
    // most MAX_CAPTURE_BYTES and discards the rest, and reports back over a
    // channel so the join below can have a deadline.
    fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let mut chunk = [0u8; 16 * 1024];
                while let Ok(n) = r.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let room = (MAX_CAPTURE_BYTES as usize).saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            let _ = tx.send(buf);
        });
        rx
    }
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());

    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if start.elapsed() >= timeout => {
                kill_process_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    // The child is gone, but a descendant it left behind can still hold the
    // pipes open: reap the group, then wait only briefly for the readers.
    kill_process_group(child.id());
    let collect = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
    };
    let stdout = collect(out_rx);
    let stderr = collect(err_rx);
    let elapsed = start.elapsed();

    let code = status.map_or(-1, |s| s.code().unwrap_or(-1));
    emit.done(code, elapsed, stdout.len() as u64, stderr.len() as u64);

    if status.is_none() {
        return Err(ProcessError::Timeout {
            program: program.to_string(),
            secs: timeout.as_secs(),
        });
    }
    Ok(Captured {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
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

#[cfg(test)]
mod capture_timeout_tests {
    use super::*;

    fn quiet() -> Emit<'static> {
        Emit::quiet()
    }

    #[test]
    fn returns_output_of_a_command_that_finishes() {
        let out = capture_with_timeout(
            "sh",
            &["-c", "echo hi; echo err >&2"],
            Path::new("/"),
            quiet(),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(
            (out.code, out.stdout.trim(), out.stderr.trim()),
            (0, "hi", "err")
        );
    }

    #[test]
    fn kills_a_command_that_outlives_the_limit() {
        let t = Instant::now();
        let err = capture_with_timeout(
            "sleep",
            &["30"],
            Path::new("/"),
            quiet(),
            Duration::from_millis(200),
        )
        .err()
        .expect("must time out");
        assert!(matches!(err, ProcessError::Timeout { .. }));
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn timeout_kills_grandchildren_holding_the_pipes() {
        let t = Instant::now();
        let err = capture_with_timeout(
            "sh",
            &["-c", "sleep 30 & wait"],
            Path::new("/"),
            quiet(),
            Duration::from_millis(200),
        )
        .err()
        .expect("must time out");
        assert!(matches!(err, ProcessError::Timeout { .. }));
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = capture_with_timeout(
            "sh",
            &["-c", "head -c 2000000 /dev/zero | tr '\\0' x"],
            Path::new("/"),
            quiet(),
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(out.stdout.len(), 2_000_000);
    }
}
