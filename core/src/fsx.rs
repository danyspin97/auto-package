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
}
