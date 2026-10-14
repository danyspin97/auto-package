//! Process-group helpers for children spawned with `process_group(0)`.
//!
//! A child that leads its own group can leave descendants behind, so the whole
//! group is killed rather than the one pid. That is only safe while the group
//! id still names that group: once the leader has been reaped and nothing is
//! left in the group, the kernel may hand the number to an unrelated process.
//! So a caller detects the leader's exit with [`has_exited`] or [`wait_exited`],
//! which leave it a zombie that still owns its pid, sweeps the group with
//! [`kill_group`], and only then reaps it (`Child::wait`).

use std::io;

/// SIGKILL the process group led by `pid`. Best effort: the group may already
/// be gone.
///
/// A group id of 0 or -1 would address the caller's own group or every process
/// the caller may signal, so only a positive pid is ever passed on.
pub fn kill_group(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    if pid <= 0 {
        return;
    }
    // SAFETY: `kill` only sends a signal; the negative pid names the process
    // group the child was placed in at spawn (`process_group(0)`).
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

/// Whether the child `pid` has exited, without reaping it.
///
/// `Ok(true)` leaves a zombie that still owns `pid`: [`kill_group`] is safe
/// until the caller reaps it.
pub fn has_exited(pid: u32) -> io::Result<bool> {
    wait_nowait(pid, libc::WNOHANG)
}

/// Block until the child `pid` has exited, without reaping it.
pub fn wait_exited(pid: u32) -> io::Result<()> {
    wait_nowait(pid, 0).map(|_| ())
}

fn wait_nowait(pid: u32, extra_flags: libc::c_int) -> io::Result<bool> {
    let id = libc::id_t::from(pid);
    loop {
        // SAFETY: an all-zero `siginfo_t` is a valid value to pass for the
        // kernel to fill in.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, writable `siginfo_t` for the whole call.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | extra_flags,
            )
        };
        if rc == 0 {
            // SAFETY: after a successful `waitid` the fields are initialised.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Command;

    #[test]
    fn exit_is_seen_without_reaping_so_the_group_can_still_be_swept() {
        // The leader exits at once and leaves a descendant in its group.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        wait_exited(child.id()).unwrap();
        assert!(has_exited(child.id()).unwrap());
        // Not reaped yet: the leader still owns the pid, so the sweep is safe.
        kill_group(child.id());
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn a_running_child_has_not_exited() {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        assert!(!has_exited(child.id()).unwrap());
        kill_group(child.id());
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn non_positive_group_ids_are_never_signalled() {
        // 0 would be our own group; the guard must make this a no-op.
        kill_group(0);
        kill_group(u32::MAX);
    }
}
