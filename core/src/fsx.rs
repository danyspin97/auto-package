//! Filesystem helpers shared by the pipeline, workspace and notes code.
//!
//! Each one replaces a file or link in a single rename, so a reader sees either
//! the old content or the new content, never a partial one or a missing link.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Prefix of the temp files [`write_atomic`] creates, so a leftover from a
/// process killed before the rename can be told apart and swept
/// ([`sweep_stale_temp_files`]).
pub const TEMP_PREFIX: &str = ".ap-tmp-";

/// Write `contents` to `path` atomically: the bytes go to a temp file in the
/// same directory, which is synced and then renamed over `path`. An existing
/// file keeps its permission bits; a new one gets 0644 rather than the 0600 a
/// temp file is created with.
///
/// If `path` is a symlink to an existing file, the file it points at is
/// replaced and the link kept: renaming over the link would silently turn it
/// into a regular file. A dangling link is replaced like any other path.
///
/// The temp file is named with [`TEMP_PREFIX`]; a kill between its creation
/// and the rename leaves it behind, and callers that publish a directory
/// sweep those with [`sweep_stale_temp_files`].
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let resolved: PathBuf;
    let path = if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        match std::fs::canonicalize(path) {
            Ok(real) => {
                resolved = real;
                resolved.as_path()
            }
            Err(_) => path,
        }
    } else {
        path
    };
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let mut tmp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(dir.unwrap_or_else(|| Path::new(".")))?;
    std::io::Write::write_all(&mut tmp, contents.as_ref())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o7777)
            .unwrap_or(0o644);
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode))?;
    }
    // The rename must not outlive the data: without this, a crash after the
    // rename can leave `path` pointing at an empty or partial file.
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Delete the [`write_atomic`] temp files in `dir` (not below it) last modified
/// more than `older_than` ago. Younger ones may belong to a live writer. Best
/// effort: returns how many were removed.
pub fn sweep_stale_temp_files(dir: &Path, older_than: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= older_than);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Point `link` at `target`, replacing whatever is there, in one rename. A
/// temp link is created beside `link` first, so there is never a moment with no
/// link at all, and two writers never collide on one temp name.
#[cfg(unix)]
pub fn replace_symlink(target: &Path, link: &Path) -> io::Result<()> {
    let dir = link.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        link.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, link) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Bytes a process that is not root can still write on the filesystem holding
/// `dir` (`statvfs`'s available blocks). `None` when the call fails, so a
/// filesystem that cannot answer never blocks work. A directory not created yet
/// is answered for by the nearest ancestor that exists.
#[cfg(unix)]
pub fn free_bytes(dir: &Path) -> Option<u64> {
    dir.ancestors()
        .filter(|d| !d.as_os_str().is_empty())
        .find_map(statvfs_free)
}

#[cfg(unix)]
fn statvfs_free(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `st` a writable
    // statvfs the call fills in.
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(not(unix))]
pub fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Whether a heavy write may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Go,
    Wait,
}

/// `Wait` when `free_bytes` is below `min_bytes`; a `min_bytes` of 0 turns the
/// guard off.
pub fn disk_gate(free_bytes: u64, min_bytes: u64) -> Gate {
    if free_bytes < min_bytes {
        Gate::Wait
    } else {
        Gate::Go
    }
}

/// Wait until the disk has `min_bytes` free. `free` reads the free space (`None`
/// is "unknown", which lets the work go); `on_wait` is told the free bytes each
/// time the gate holds; `sleep` pauses between checks and returns false to stop
/// waiting (a shutdown), which lets the work go on to be stopped by its own
/// checks. The disk is checked `limit / interval + 1` times, `interval` apart;
/// `Err(free)` is the free space of the last check when it never recovered.
pub fn wait_for_space(
    min_bytes: u64,
    interval: Duration,
    limit: Duration,
    mut free: impl FnMut() -> Option<u64>,
    mut on_wait: impl FnMut(u64),
    mut sleep: impl FnMut(Duration) -> bool,
) -> Result<(), u64> {
    let checks = if interval.is_zero() {
        1
    } else {
        limit.as_nanos() / interval.as_nanos() + 1
    };
    for check in 0..checks {
        let Some(now) = free() else {
            return Ok(());
        };
        if disk_gate(now, min_bytes) == Gate::Go {
            return Ok(());
        }
        if check + 1 == checks {
            return Err(now);
        }
        on_wait(now);
        if !sleep(interval) {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "old and longer").unwrap();
        write_atomic(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_keeps_the_mode_of_an_existing_file_and_defaults_new_ones() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&path, "new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);

        let fresh = dir.path().join("g.txt");
        write_atomic(&fresh, "new").unwrap();
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_through_a_symlink_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, "old").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, "new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    }

    #[test]
    fn the_sweep_removes_only_old_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join(format!("{TEMP_PREFIX}old"));
        let other = dir.path().join("keep.txt");
        std::fs::write(&old, "x").unwrap();
        std::fs::write(&other, "x").unwrap();
        assert_eq!(
            sweep_stale_temp_files(dir.path(), Duration::from_secs(3600)),
            0
        );
        assert_eq!(sweep_stale_temp_files(dir.path(), Duration::ZERO), 1);
        assert!(!old.exists() && other.exists());
    }

    #[cfg(unix)]
    #[test]
    fn replace_symlink_moves_an_existing_link_without_leaving_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("latest");
        std::os::unix::fs::symlink("run-1", &link).unwrap();
        replace_symlink(Path::new("run-2"), &link).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("run-2"));
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "no temp link may be left behind");
    }

    #[test]
    fn the_gate_holds_below_the_minimum_and_zero_disables_it() {
        assert_eq!(disk_gate(5, 10), Gate::Wait);
        assert_eq!(disk_gate(10, 10), Gate::Go);
        assert_eq!(disk_gate(0, 0), Gate::Go);
    }

    #[test]
    fn free_bytes_reads_a_real_directory_and_a_missing_one_by_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_bytes(dir.path()).is_some_and(|b| b > 0));
        assert!(free_bytes(&dir.path().join("absent/deeper")).is_some_and(|b| b > 0));
    }

    #[test]
    fn waiting_stops_as_soon_as_space_returns() {
        let mut readings = vec![Some(9), Some(8), Some(50)].into_iter();
        let (mut waits, mut sleeps) = (Vec::new(), 0);
        let r = wait_for_space(
            10,
            Duration::from_secs(30),
            Duration::from_secs(600),
            || readings.next().unwrap(),
            |f| waits.push(f),
            |_| {
                sleeps += 1;
                true
            },
        );
        assert_eq!(r, Ok(()));
        assert_eq!((waits, sleeps), (vec![9, 8], 2));
    }

    #[test]
    fn waiting_gives_up_after_the_limit_with_the_last_free_space() {
        let (mut waits, mut sleeps) = (0, 0);
        let r = wait_for_space(
            10,
            Duration::from_secs(30),
            Duration::from_secs(600),
            || Some(1),
            |_| waits += 1,
            |d| {
                assert_eq!(d, Duration::from_secs(30));
                sleeps += 1;
                true
            },
        );
        assert_eq!(r, Err(1));
        // 21 checks, 20 pauses of 30 s = 10 minutes.
        assert_eq!((waits, sleeps), (20, 20));
    }

    #[test]
    fn waiting_does_not_hold_for_an_unknown_or_ample_disk_or_a_shutdown() {
        let ok = |free: Option<u64>, keep_sleeping: bool| {
            wait_for_space(
                10,
                Duration::from_secs(1),
                Duration::from_secs(5),
                || free,
                |_| {},
                |_| keep_sleeping,
            )
        };
        assert_eq!(ok(None, true), Ok(()));
        assert_eq!(ok(Some(10), true), Ok(()));
        assert_eq!(ok(Some(1), false), Ok(()));
    }
}
