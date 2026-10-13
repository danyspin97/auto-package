//! Advisory `flock(2)` lock files shared by the notes and project locks.
//!
//! The kernel drops the lock when the owning process exits, however it exits,
//! so a crashed run never leaves a lock that needs taking over, and two runs
//! can never both believe they hold it.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another open file description holds it. Carries the pid that holder
    /// wrote, when it has written one yet.
    Held(Option<u32>),
    Io(io::Error),
}

/// Take an exclusive, non-blocking lock on `path`, creating it if needed. The
/// returned file must be kept open for as long as the lock should be held;
/// closing it releases the lock.
///
/// The file is never deleted by its owner: unlinking a locked file lets a
/// second run lock the new file at the same path while the first still holds
/// the old one.
pub fn try_lock(path: &Path) -> Result<File, LockError> {
    // No truncation on open: that would wipe the holder's pid before we know
    // whether we got the lock.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(LockError::Io)?;
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
}
