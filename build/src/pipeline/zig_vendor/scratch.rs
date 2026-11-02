//! A directory the vendoring step creates for its temporary files, removed
//! when it is dropped, on success and on failure. The package's work
//! directory holds it, never `/tmp`: a caller makes one under the package's
//! work directory and drops it when the vendor archive is written or the
//! fetch has failed.

use std::path::{Path, PathBuf};

/// A directory that did not exist before, removed with everything in it when
/// dropped.
#[derive(Debug)]
pub(super) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Create `path`, whose parent must exist. A path that already exists is an
    /// error, so only a directory this type made is ever removed.
    pub(super) fn create(path: PathBuf) -> Result<Scratch, String> {
        std::fs::create_dir(&path).map_err(|e| format!("creating {}: {e}", path.display()))?;
        Ok(Scratch { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
