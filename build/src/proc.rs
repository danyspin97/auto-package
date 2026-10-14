//! Subprocess execution with captured output and event emission.

use std::collections::VecDeque;
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
    /// A stream could not be read to its end, so an empty or short capture
    /// would be indistinguishable from real output.
    #[error("output of {program} could not be read completely ({stream})")]
    OutputLost {
        program: String,
        stream: &'static str,
    },
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

/// Wall-clock limit applied by [`capture`] when the caller has no tighter one:
/// generous enough for `osc service ra` or a large `osc ci`, finite so a
/// stalled connection cannot hang the run forever.
pub const DEFAULT_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Cap on what one captured stream keeps. A chatty child is still drained
/// (so it never blocks on a full pipe) but only the last this-many bytes are
/// kept: a failing build's cause is at the end of its log.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

/// A directory only this process can use, for scratch files and as the working
/// directory of helpers that read configuration from the directory they start
/// in (`gh` and `git` look for `.git`, `osc` for `.osc`).
///
/// The shared temporary directory is writable by every local user, so a path
/// there can be pre-created by someone else (a predictable name, a planted
/// `.git`), and a symlink or a hostile file in it is read as if it were ours.
/// This one is created once, with a random name and mode 0700. If it cannot be
/// created the root directory is returned, which holds nothing of that kind
/// and nobody else can write to.
pub fn private_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .prefix("auto-package-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map(tempfile::TempDir::keep)
            .unwrap_or_else(|_| PathBuf::from("/"))
    })
}

/// Remove [`private_dir`] and everything in it. Best effort, for process exit.
pub fn remove_private_dir() {
    let dir = private_dir();
    if dir != Path::new("/") {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Append `chunk` to `buf`, then drop from the front so at most `cap` bytes
/// remain — the tail of everything written so far.
fn push_tail(buf: &mut VecDeque<u8>, chunk: &[u8], cap: usize) {
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

/// Set once [`kill_all_groups`] runs. A child spawned just before Ctrl-C but
/// not yet registered is missed by that sweep, and being in its own group it
/// would be orphaned; `track_group` checks this flag so it is killed on
/// registration instead.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record `pid` (spawned with `process_group(0)`) as running until the guard drops.
///
/// When shutdown has begun the group is killed at once and an error returned,
/// so the caller must reap the child and give up.
pub(crate) fn track_group(pid: u32) -> io::Result<GroupGuard> {
    // The flag is checked under the lock that `kill_all_groups` takes after
    // setting it: either the sweep sees this pid, or this sees the flag.
    let mut groups = ACTIVE_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
    if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
        drop(groups);
        kill_process_group(pid);
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "shutting down; child killed",
        ));
    }
    groups.insert(pid);
    Ok(GroupGuard(pid))
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
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
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

/// SIGKILL the group `pid` leads. Only while the leader is still unreaped (see
/// [`ap_core::pgroup`]): after that the number may belong to another group.
pub(crate) fn kill_process_group(pid: u32) {
    ap_core::pgroup::kill_group(pid);
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
    let collect = |rx: std::sync::mpsc::Receiver<(Vec<u8>, bool)>| match rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        Ok((buf, false)) => Some(buf),
        _ => None,
    };
    let stdout = collect(out_rx);
    let stderr = collect(err_rx);
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

    #[cfg(unix)]
    #[test]
    fn kill_process_group_sigkills_a_child_started_in_its_own_group() {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        kill_process_group(child.id());
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn unreadable_output_is_an_error_not_an_empty_success() {
        // The child exits at once but a background descendant in another
        // session keeps both pipes open, so neither stream is ever known.
        let err = capture_with_timeout(
            "sh",
            &["-c", "setsid sleep 8 & exit 0"],
            Path::new("/"),
            quiet(),
            Duration::from_secs(20),
        )
        .err()
        .expect("must not be Ok");
        assert!(matches!(err, ProcessError::OutputLost { .. }));
    }

    #[test]
    fn clean_exit_keeps_a_stream_that_was_read_in_full() {
        // stdout reaches EOF; only stderr is held open by the escaped sleep.
        // (the shell's own stdout pipe closes on exit, sleep's goes to null)
        let out = capture_with_timeout(
            "sh",
            &["-c", "echo hi; setsid sleep 8 >/dev/null & exit 0"],
            Path::new("/"),
            quiet(),
            Duration::from_secs(20),
        )
        .expect("complete stdout of a clean exit is returned");
        assert_eq!(out.code, 0);
        assert_eq!(out.stdout, "hi\n");
    }

    /// Scratch files and the working directory of `gh`/`osc` live in a
    /// directory only this user can enter, not at a predictable shared path.
    #[test]
    fn private_dir_is_a_fresh_mode_0700_directory_under_the_temp_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_dir();
        let meta = std::fs::symlink_metadata(dir).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert!(dir.starts_with(std::env::temp_dir()));
        // The same directory every time within one process.
        assert_eq!(dir, private_dir());
    }

    #[test]
    fn push_tail_keeps_only_the_last_cap_bytes() {
        let mut buf = VecDeque::new();
        push_tail(&mut buf, b"abcdef", 4);
        push_tail(&mut buf, b"gh", 4);
        assert_eq!(Vec::from(buf.clone()), b"efgh");
        push_tail(&mut buf, b"ij", 4);
        assert_eq!(Vec::from(buf), b"ghij");
    }

    #[test]
    fn a_stream_larger_than_the_cap_keeps_its_end() {
        let mut buf = VecDeque::new();
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        for chunk in data.chunks(777) {
            push_tail(&mut buf, chunk, 1_000);
        }
        assert_eq!(Vec::from(buf), &data[data.len() - 1_000..]);
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
