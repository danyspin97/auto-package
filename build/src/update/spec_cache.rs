//! The spec texts of the project's packages, kept per OBS revision in the work
//! directory, so a repeat plan (a dry run, then the real run) reads a package's
//! spec again only when its revision moved.
//!
//! A plan asks OBS once for the revisions of all its packages (see
//! [`known_revisions`]). A spec is served from the work directory while the
//! package's revision is the one it was kept under. A spec is kept only when
//! the revision read after the spec read is the one before it, so the text is
//! that revision's; the "after" revision is asked per package, and only on a
//! miss. A package the bulk answer does not give (a broken link, no spec of its
//! own, OBS unavailable) is asked per package. A failed read is never kept, and
//! a cache file that does not read is a miss. Without a work directory (see
//! `upstream_cache::set_dir`) nothing is asked, read or written beyond the
//! plain spec read.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The directory of the work directory that holds the cached specs.
const DIR: &str = "spec-cache";

#[derive(Serialize, Deserialize)]
struct Entry {
    revision: String,
    spec: String,
}

/// The revisions of the project's packages, asked once for the plan. Empty
/// when there is no work directory (nothing is cached then) or OBS did not
/// answer: each package is then asked for on its own.
pub(super) fn known_revisions(project: &str) -> HashMap<String, String> {
    match upstream_cache::work_dir() {
        None => HashMap::new(),
        Some(_) => osc::project_spec_revisions(project).unwrap_or_default(),
    }
}

/// A file name for `name`: ASCII letters, digits, `-` and `.` kept, every other
/// byte (`:`, `+`, `_`, `%`, `/`) written as `%XX`. Distinct names give distinct
/// files, and no name leaves its directory: the one dot names `.` and `..` are
/// written out as well.
pub(super) fn file_part(name: &str) -> String {
    let dots_only = name == "." || name == "..";
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || (b == b'-') || (b == b'.' && !dots_only) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The file that holds the spec of `pkg` in `project`, under `work_dir`.
pub(super) fn path_of(work_dir: &Path, project: &str, pkg: &str) -> PathBuf {
    work_dir
        .join(DIR)
        .join(file_part(project))
        .join(format!("{}.json", file_part(pkg)))
}

/// The entry at `path`, when it reads.
fn load(path: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Keep `spec` under `revision` at `path`. Best effort: a cache that cannot be
/// written only costs the next plan a read.
fn store(path: &Path, revision: &str, spec: &str) {
    let entry = Entry {
        revision: revision.to_string(),
        spec: spec.to_string(),
    };
    let Ok(text) = serde_json::to_string(&entry) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = ap_core::fsx::write_atomic(path, text);
}

/// The spec at `path`, served when it was kept under the package's revision and
/// read otherwise. `known` is the revision the plan already has (`None` asks
/// `now`), and `now` asks OBS for the revision at the moment it is called; a
/// `None` from either means the revision is not known and the spec is read and
/// not kept.
fn cached_read(
    path: &Path,
    known: Option<String>,
    now: &dyn Fn() -> Option<String>,
    read: &dyn Fn() -> Result<String, ReadError>,
) -> Result<String, ReadError> {
    let Some(rev) = known.or_else(now) else {
        return read();
    };
    if let Some(entry) = load(path)
        && entry.revision == rev
    {
        return Ok(entry.spec);
    }
    let spec = read()?;
    // The revision read after the spec must still be the one before it: only
    // then is the spec the text of that revision.
    if now().as_deref() == Some(rev.as_str()) {
        store(path, &rev, &spec);
    }
    Ok(spec)
}

/// [`cached_read`] for package `pkg` of `project`, or the plain read when there
/// is no work directory.
pub(super) fn spec_with(
    work_dir: Option<&Path>,
    project: &str,
    pkg: &str,
    known: Option<String>,
    now: &dyn Fn() -> Option<String>,
    read: &dyn Fn() -> Result<String, ReadError>,
) -> Result<String, ReadError> {
    match work_dir {
        None => read(),
        Some(dir) => cached_read(&path_of(dir, project, pkg), known, now, read),
    }
}

/// The spec of OBS package `pkg` in `project`, through the work directory's
/// cache when there is one (see the module doc). `known` is what
/// [`known_revisions`] gave.
pub(super) fn spec_for_package(
    project: &str,
    pkg: &str,
    known: &HashMap<String, String>,
    emit: Emit<'_>,
) -> Result<String, ReadError> {
    spec_with(
        upstream_cache::work_dir(),
        project,
        pkg,
        known.get(pkg).cloned(),
        &|| osc::package_spec_revision(project, pkg),
        &|| read::spec_for_package(project, pkg, emit),
    )
}

#[cfg(test)]
#[path = "spec_cache_tests.rs"]
mod tests;
