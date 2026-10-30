//! A child run to its end with its output written to a log file as it comes.
//!
//! For a build that may run for hours: a timeout keeps what the child printed,
//! and the child starts a session of its own, so it has no controlling
//! terminal. A credential prompt that reads `/dev/tty` then fails at once,
//! instead of stopping the child on a terminal read nobody can answer.

use super::*;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;

/// Run `program args` in `cwd` with its stdout and stderr appended to `log`.
///
/// Returns the exit code (`-1` when it was killed by a signal), or `None` when
/// the child was still running after `timeout`: it then gets SIGTERM to its
/// process group, `grace` to exit, and SIGKILL after that. The log is written
/// through [`write_log`] at the end, so it is redacted and private like every
/// other command log.
pub fn run_logged(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    log: &Path,
    emit: Emit<'_>,
    timeout: Duration,
    grace: Duration,
) -> Result<Option<i32>, ProcessError> {
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();
    emit.started(program, &argv, cwd, Some(log));
    let start = Instant::now();

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(log)?;
    let stderr = file.try_clone()?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(stderr));
    // SAFETY: `setsid` only makes the child a session leader with no
    // controlling terminal. It touches no memory and is async-signal-safe.
    // Its session id is also its process group id, so the group kills below
    // reach the child's descendants.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| ProcessError::Launch {
        program: program.to_string(),
        source: e,
    })?;
    let mut group = match track_group(child.id()) {
        Ok(g) => Some(g),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
    };

    let code = loop {
        match ap_core::pgroup::has_exited(child.id()) {
            Ok(true) => {
                kill_process_group(child.id());
                drop(group.take());
                break Some(child.wait()?.code().unwrap_or(-1));
            }
            Ok(false) if start.elapsed() >= timeout => {
                signal_group(child.id(), libc::SIGTERM);
                let stop = Instant::now();
                while stop.elapsed() < grace
                    && !matches!(ap_core::pgroup::has_exited(child.id()), Ok(true))
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                kill_process_group(child.id());
                drop(group.take());
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                kill_process_group(child.id());
                drop(group.take());
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
    };
    let written = std::fs::metadata(log).map_or(0, |m| m.len());
    emit.done(code.unwrap_or(-1), start.elapsed(), written, 0);
    let text = std::fs::read(log)?;
    write_log(log, &text)?;
    Ok(code)
}

/// Send `sig` to the process group `pid` leads. Errors are ignored: a group
/// that has already gone has nothing left to signal.
fn signal_group(pid: u32, sig: libc::c_int) {
    if let Ok(pid) = libc::pid_t::try_from(pid)
        && pid > 0
    {
        // SAFETY: `kill` only sends a signal to the group the child leads.
        unsafe {
            libc::kill(-pid, sig);
        }
    }
}
