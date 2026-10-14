//! Advisory `flock(2)` lock files shared by the notes and project locks.
//!
//! The kernel drops the lock when the owning process exits, however it exits,
//! so a crashed run never leaves a lock that needs taking over, and two runs
//! can never both believe they hold it.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another open file description holds it. Carries the pid that holder
    /// wrote, when it has written one yet.
    Held(Option<u32>),
    Io(io::Error),
}

/// Like [`try_lock`], but keeps retrying for up to `wait` while another holder
/// has the lock. The poll interval is short so a short hold costs little.
pub fn lock_with_wait(path: &Path, wait: std::time::Duration) -> Result<File, LockError> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match try_lock(path) {
            Err(LockError::Held(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            other => return other,
        }
    }
}

/// Take an exclusive, non-blocking lock on `path`, creating it if needed. The
/// returned file must be kept open for as long as the lock should be held;
/// closing it releases the lock.
///
/// The file is never deleted by its owner: unlinking a locked file lets a
/// second run lock the new file at the same path while the first still holds
/// the old one.
///
/// The path is opened with `O_NOFOLLOW` and must be a regular file: a lock
/// path is writable and gets truncated and rewritten, so a symlink planted
/// there (the lock directories can sit in a shared location) must not turn
/// that into a write to whatever it points at.
pub fn try_lock(path: &Path) -> Result<File, LockError> {
    // No truncation on open: that would wipe the holder's pid before we know
    // whether we got the lock.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(LockError::Io)?;
    // Checked on the open descriptor, before anything is truncated.
    let meta = file.metadata().map_err(LockError::Io)?;
    if !meta.is_file() {
        return Err(LockError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        )));
    }
    // SAFETY: `file` owns a valid descriptor for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            return Err(LockError::Held(text.trim().parse().ok()));
        }
        return Err(LockError::Io(err));
    }
    file.set_len(0).map_err(LockError::Io)?;
    write!(file, "{}", std::process::id()).map_err(LockError::Io)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_in_the_same_process_is_refused_and_names_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("l.lock");
        let first = try_lock(&path).unwrap();
        match try_lock(&path) {
            Err(LockError::Held(pid)) => assert_eq!(pid, Some(std::process::id())),
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        try_lock(&path).unwrap();
    }

    /// A symlink at the lock path must not be followed: the lock file is
    /// truncated and rewritten, which would clobber the link's target.
    #[test]
    fn a_symlinked_lock_file_is_refused_and_its_target_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("victim");
        std::fs::write(&target, "precious").unwrap();
        let path = dir.path().join("l.lock");
        std::os::unix::fs::symlink(&target, &path).unwrap();

        match try_lock(&path) {
            Err(LockError::Io(_)) => {}
            other => panic!("expected an I/O error, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "precious");
    }

    #[test]
    fn a_lock_path_that_is_not_a_regular_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        // A directory cannot be opened for writing at all.
        assert!(matches!(try_lock(dir.path()), Err(LockError::Io(_))));
        // A FIFO opens, but is not a lock file.
        let fifo = dir.path().join("fifo.lock");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(matches!(try_lock(&fifo), Err(LockError::Io(_))));
    }
}
