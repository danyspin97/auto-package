//! The byte edit a refresh turn asks for, checked before a file is touched.
//!
//! The turn names three workspace-relative paths: the file to edit, which must
//! be under the workspace's `source/` (the new sources), and two scratch files
//! holding the text to replace and its replacement. The scratch files are under
//! the workspace but not under `source/`, and hold UTF-8 text, since the turn
//! writes them with its own Write tool. A path is checked as written (no
//! absolute path, no `..`) and then as resolved: a symlink that leads outside
//! its allowed directory is refused.

use super::{Error, replace_once};
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
mod tests;

/// Edits `file` under `ws/source/` by replacing the one occurrence of the text
/// in `old_file` with the text in `new_file`. All three paths are relative to
/// `ws`. Returns the file's resolved path.
pub fn edit_in_workspace(
    ws: &Path,
    file: &Path,
    old_file: &Path,
    new_file: &Path,
) -> Result<PathBuf, Error> {
    let ws = canonical(ws)?;
    let source = canonical(&ws.join("source"))?;
    let target = resolve_under(&ws, file, &source)?;
    if target.strip_prefix(&source).is_ok_and(has_pc_component) {
        return Err(refused(
            file,
            "is quilt's own state (`.pc`), not a source file",
        ));
    }
    if !target.is_file() {
        return Err(refused(file, "is not a regular file"));
    }
    let old = scratch_text(&ws, &source, old_file)?;
    let new = scratch_text(&ws, &source, new_file)?;
    replace_once(&target, &old, &new)?;
    Ok(target)
}

/// The text of a scratch file: a file under the workspace and not under
/// `source/`, holding UTF-8 text.
fn scratch_text(ws: &Path, source: &Path, file: &Path) -> Result<Vec<u8>, Error> {
    let path = resolve_under(ws, file, ws)?;
    if path.starts_with(source) {
        return Err(refused(
            file,
            "is under source/; scratch text is kept outside it",
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| refused(file, &format!("cannot be read: {e}")))?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err(refused(file, "is not UTF-8 text"));
    }
    Ok(bytes)
}

/// `file` resolved under `root`, where `file` is relative to `ws`. Refused
/// when it is absolute or has a `..` or prefix component, when it does not
/// exist, or when it resolves (through a symlink) outside `root`.
fn resolve_under(ws: &Path, file: &Path, root: &Path) -> Result<PathBuf, Error> {
    let written_ok = !file.as_os_str().is_empty()
        && file
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !written_ok {
        return Err(refused(
            file,
            "must be a relative path with no `..` and no leading `/`",
        ));
    }
    let resolved = canonical(&ws.join(file)).map_err(|_| refused(file, "does not exist"))?;
    if !resolved.starts_with(root) || resolved == root {
        return Err(refused(
            file,
            &format!("resolves outside {}", root.display()),
        ));
    }
    Ok(resolved)
}

fn has_pc_component(rel: &Path) -> bool {
    rel.components().any(|c| c.as_os_str() == ".pc")
}

fn canonical(path: &Path) -> Result<PathBuf, Error> {
    std::fs::canonicalize(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

fn refused(file: &Path, why: &str) -> Error {
    Error::Refused(format!("{}: {why}", file.display()))
}
