//! The snapshot of a checkout that a turn is judged against.

use super::*;

/// Validate an agent's spec edit, keep it or restore `before`, and report.
///
/// Returns true when the edit was kept. `rpmspec --parse` is the judge: a
/// macro reference inside a comment is only warned about, because RPM expands
/// comments but the great majority of such lines still parse fine, and
/// throwing away an otherwise good fix costs an attempt.
/// Snapshot of the top-level files in a checkout directory before an agent
/// turn runs, so a reject can roll back the agent's changes to the files it
/// may edit: spec, patches, `.changes`, `series`, and `_service`.
///
/// Only those editable files have their contents kept (and only when small).
/// Everything else at the checkout root — source archives, which can be
/// hundreds of megabytes — is recorded by size and mtime alone and is never
/// read. The `.osc/` metadata directory and the extracted source tree
/// (potentially millions of files) are deliberately NOT snapshotted.
pub(super) struct CheckoutSnapshot {
    pub(super) checkout_dir: PathBuf,
    /// filename → what the file looked like at snapshot time
    pub(super) files: std::collections::HashMap<std::ffi::OsString, SnapshotEntry>,
}

/// Largest editable file whose contents are kept for rollback.
const SNAPSHOT_CONTENT_CAP: u64 = 16 * 1024 * 1024;

pub(super) struct SnapshotEntry {
    len: u64,
    modified: Option<std::time::SystemTime>,
    /// Contents, kept only for editable files under [`SNAPSHOT_CONTENT_CAP`].
    pub(super) content: Option<Vec<u8>>,
}

/// Top-level files whose contents are kept: the files the agent is allowed to
/// change, and the refresh series the pipeline writes before a snapshot is
/// taken (see `write_rebase_series`), which a rollback must be able to put back.
fn is_agent_editable(name: &str) -> bool {
    name == "series"
        || name == crate::agent::REFRESH_SERIES
        || name == "_service"
        || [".spec", ".patch", ".diff", ".changes"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

/// Whether a rollback leaves the top-level file `name` as it is: it was staged
/// by a turn (named in `keep`). A spec is never kept, whatever `keep` says: a
/// rejected spec is always restored.
fn is_kept_file(name: &std::ffi::OsStr, keep: &[String]) -> bool {
    name.to_str()
        .is_some_and(|n| !n.ends_with(".spec") && keep.iter().any(|k| k == n))
}

impl CheckoutSnapshot {
    pub(super) fn take(checkout_dir: &Path) -> Self {
        let mut files = std::collections::HashMap::new();
        if let Ok(iter) = std::fs::read_dir(checkout_dir) {
            for entry in iter.filter_map(|e| e.ok()) {
                let ty = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if !ty.is_file() {
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let name = entry.file_name();
                let editable = name.to_str().is_some_and(is_agent_editable);
                let content = if editable && meta.len() <= SNAPSHOT_CONTENT_CAP {
                    std::fs::read(entry.path()).ok()
                } else {
                    None
                };
                files.insert(
                    name,
                    SnapshotEntry {
                        len: meta.len(),
                        modified: meta.modified().ok(),
                        content,
                    },
                );
            }
        }
        Self {
            checkout_dir: checkout_dir.to_path_buf(),
            files,
        }
    }

    /// The bytes of `name` when the snapshot was taken, when they were kept.
    pub(super) fn content_of(&self, name: &str) -> Option<Vec<u8>> {
        self.files
            .get(std::ffi::OsStr::new(name))
            .and_then(|e| e.content.clone())
    }

    /// Whether `name` is in the snapshot with exactly these bytes. A file whose
    /// contents were not kept is never reported unchanged.
    pub(super) fn holds_unchanged(&self, name: &str, bytes: &[u8]) -> bool {
        matches!(
            self.files.get(std::ffi::OsStr::new(name)),
            Some(SnapshotEntry { content: Some(old), .. }) if old.as_slice() == bytes
        )
    }

    /// Put every top-level `.changes` file back to its snapshot state, and
    /// delete one the turn added. An accepted turn keeps its spec edit but not
    /// its `.changes` text: that flows into the commit message and the request
    /// description, so only the pipeline's own entry may land there.
    pub(super) fn restore_changes_files(&self, reporter: &dyn Reporter, step: Option<&StepDir>) {
        let step_id = step.map(|s| s.id);
        let warn = |message: String| {
            reporter.report(&Event::Warning {
                step: step_id,
                message,
            });
        };
        for (name, entry) in &self.files {
            if !name.to_str().is_some_and(|n| n.ends_with(".changes")) {
                continue;
            }
            let path = self.checkout_dir.join(name);
            match &entry.content {
                Some(bytes) => {
                    if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice())
                        && let Err(e) = ap_core::fsx::write_atomic(&path, bytes)
                    {
                        warn(format!("could not restore {}: {e}", path.display()));
                    }
                }
                None => warn(format!(
                    "cannot roll back {}: its contents were not snapshotted",
                    path.display()
                )),
            }
        }
        if let Ok(iter) = std::fs::read_dir(&self.checkout_dir) {
            for entry in iter.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let is_new_changes = entry.file_type().is_ok_and(|t| t.is_file())
                    && name.to_str().is_some_and(|n| n.ends_with(".changes"))
                    && !self.files.contains_key(&name);
                if is_new_changes && let Err(e) = std::fs::remove_file(entry.path()) {
                    warn(format!(
                        "could not delete agent-added file {}: {e}",
                        entry.path().display()
                    ));
                }
            }
        }
    }

    /// Restore every top-level file to its snapshot state: restore
    /// modified files, re-create deleted ones, and delete any top-level
    /// files the agent added. Idempotent — called on reject only.
    ///
    /// A file whose contents already match is never rewritten, so its mtime
    /// and inode stay as they were.
    pub(super) fn restore(&self, reporter: &dyn Reporter, step: Option<&StepDir>) {
        self.restore_keeping(reporter, step, &[]);
    }

    /// Like [`Self::restore`], but top-level files named in `keep` are left as they
    /// are: the files a turn asked to be staged, whether they are new (not in the
    /// snapshot) or overwrite a snapshotted file (see [`is_kept_file`]).
    pub(super) fn restore_keeping(
        &self,
        reporter: &dyn Reporter,
        step: Option<&StepDir>,
        keep: &[String],
    ) {
        let step_id = step.map(|s| s.id);
        // 1) Put back every snapshotted file the agent changed or deleted, except
        //    the staged ones, whose staged content stays.
        for (name, entry) in &self.files {
            if is_kept_file(name, keep) {
                continue;
            }
            let path = self.checkout_dir.join(name);
            let Some(bytes) = &entry.content else {
                // Not kept (an archive or an oversized file): all that can be
                // done is to say that it changed.
                let changed = match std::fs::symlink_metadata(&path) {
                    Ok(m) => {
                        !m.is_file() || m.len() != entry.len || m.modified().ok() != entry.modified
                    }
                    Err(_) => true,
                };
                if changed {
                    reporter.report(&Event::Warning {
                        step: step_id,
                        message: format!(
                            "cannot roll back {}: its contents were not snapshotted",
                            path.display()
                        ),
                    });
                }
                continue;
            };
            let current_matches = std::fs::symlink_metadata(&path)
                .ok()
                .filter(|m| m.is_file() && m.len() == bytes.len() as u64)
                .and_then(|_| std::fs::read(&path).ok())
                .is_some_and(|now| now == *bytes);
            if current_matches {
                continue;
            }
            if let Err(e) = ap_core::fsx::write_atomic(&path, bytes) {
                reporter.report(&Event::Warning {
                    step: step_id,
                    message: format!("could not restore {}: {e}", path.display()),
                });
            }
        }
        // 2) Delete any top-level files that didn't exist in the snapshot
        //    (i.e. new files the agent added).
        if let Ok(iter) = std::fs::read_dir(&self.checkout_dir) {
            for entry in iter.filter_map(|e| e.ok()) {
                let ty = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if !ty.is_file() {
                    continue;
                }
                let name = entry.file_name();
                if !self.files.contains_key(&name) && !is_kept_file(&name, keep) {
                    let path = entry.path();
                    if let Err(e) = std::fs::remove_file(&path) {
                        reporter.report(&Event::Warning {
                            step: step_id,
                            message: format!(
                                "could not delete agent-added file {}: {e}",
                                path.display()
                            ),
                        });
                    }
                }
            }
        }
    }
}
