//! Filesystem helpers shared by the pipeline, workspace and notes code.
//!
//! Each one replaces a file or link in a single rename, so a reader sees either
//! the old content or the new content, never a partial one or a missing link.

use std::io;
use std::path::Path;

/// Write `contents` to `path` atomically: the bytes go to a temp file in the
/// same directory, which is synced and then renamed over `path`. An existing
/// file keeps its permission bits; a new one gets 0644 rather than the 0600 a
/// temp file is created with.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let mut tmp = tempfile::NamedTempFile::new_in(dir.unwrap_or_else(|| Path::new(".")))?;
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
