//! `--cleanup`: delete what a run can rebuild, never what it cannot.
//!
//! Kept: `runs/` (the only audit record), `state/` (per-package state and
//! notes), `ledger.json`, every osc checkout and `reviews/` (the saved reviews
//! of submit requests, which sit at the root of the work directory, beside the
//! project directories this works on). Deleted: `rpms/` (downloaded and built
//! RPMs) and the unpacked sdist caches under `state/<pkg>/`. Refused outright,
//! with the checkouts listed, when an osc checkout holds edits that were never
//! committed.

use std::path::{Path, PathBuf};

/// What a cleanup removed.
#[derive(Debug, Default)]
pub struct Cleaned {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
}

/// Bytes under `path` (symlinks are counted as themselves, never followed).
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

/// Directories under `project_dir` that are osc checkouts (they hold `.osc`).
/// `state/`, `runs/` and `rpms/` are never searched.
fn checkouts(project_dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if dir.join(".osc").is_dir() {
            out.push(dir.to_path_buf());
            return;
        }
        if depth == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&p, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(project_dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        if matches!(name.to_str(), Some("state" | "runs" | "rpms")) {
            continue;
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&e.path(), 3, &mut out);
        }
    }
    out.sort();
    out
}

/// The checkout has uncommitted changes, or this cannot be told. Files
/// scheduled for addition are seen without running `osc`; the rest is
/// `osc status`, which only reads the working copy. A failing `osc` counts as
/// dirty: refusing is the safe answer.
pub fn checkout_is_dirty(dir: &Path) -> bool {
    let added = dir.join(".osc").join("_to_be_added");
    if std::fs::read_to_string(&added).is_ok_and(|t| !t.trim().is_empty()) {
        return true;
    }
    match ap_build::proc::capture("osc", &["status"], dir, ap_build::proc::Emit::quiet()) {
        Ok(out) => !out.success() || !out.stdout.trim().is_empty(),
        Err(_) => true,
    }
}

/// Clean `project_dir`. `is_dirty` decides whether a checkout has unpublished
/// edits.
pub fn cleanup(project_dir: &Path, is_dirty: impl Fn(&Path) -> bool) -> Result<Cleaned, String> {
    let dirty: Vec<PathBuf> = checkouts(project_dir)
        .into_iter()
        .filter(|d| is_dirty(d))
        .collect();
    if !dirty.is_empty() {
        let list: Vec<String> = dirty.iter().map(|d| format!("  {}", d.display())).collect();
        return Err(format!(
            "--cleanup refuses to run: these osc checkouts have uncommitted changes that \
             would be lost:\n{}\nCommit or revert them by hand first (or drop --cleanup).",
            list.join("\n")
        ));
    }

    let mut targets = vec![project_dir.join("rpms")];
    if let Ok(rd) = std::fs::read_dir(project_dir.join("state")) {
        for e in rd.flatten() {
            targets.push(e.path().join("sdist"));
            targets.push(e.path().join("sdist.stamp"));
        }
    }
    let mut cleaned = Cleaned::default();
    for t in targets {
        let Ok(meta) = std::fs::symlink_metadata(&t) else {
            continue;
        };
        let bytes = size_of(&t);
        let res = if meta.is_dir() {
            std::fs::remove_dir_all(&t)
        } else {
            std::fs::remove_file(&t)
        };
        res.map_err(|e| format!("cannot remove {}: {e}", t.display()))?;
        cleaned.bytes += bytes;
        cleaned.removed.push(t);
    }
    Ok(cleaned)
}

/// Run the cleanup and print what it removed.
pub fn run(project_dir: &Path) -> color_eyre::Result<()> {
    let c = cleanup(project_dir, checkout_is_dirty).map_err(|e| color_eyre::eyre::eyre!(e))?;
    for p in &c.removed {
        eprintln!("cleanup: deleted {}", p.display());
    }
    eprintln!(
        "cleanup: freed {} bytes ({:.1} MiB); runs/, state/ and ledger.json are kept",
        c.bytes,
        c.bytes as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for dir in [
            "rpms/x86_64",
            "runs/r1",
            "state/pkg/sdist/src",
            "home:me:py/pkg/.osc",
        ] {
            std::fs::create_dir_all(p.join(dir)).unwrap();
        }
        std::fs::write(p.join("rpms/x86_64/a.rpm"), vec![0u8; 1000]).unwrap();
        std::fs::write(p.join("state/pkg/sdist/src/f"), vec![0u8; 500]).unwrap();
        std::fs::write(p.join("state/pkg/sdist.stamp"), "x").unwrap();
        std::fs::write(p.join("state/pkg/state.json"), "{}").unwrap();
        std::fs::write(p.join("state/pkg/notes.md"), "n").unwrap();
        std::fs::write(p.join("runs/r1/run.jsonl"), "{}").unwrap();
        std::fs::write(p.join("ledger.json"), "{}").unwrap();
        std::fs::write(p.join("home:me:py/pkg/pkg.spec"), "spec").unwrap();
        d
    }

    #[test]
    fn saved_reviews_at_the_root_of_the_work_directory_are_kept() {
        let work = tempfile::tempdir().unwrap();
        let review = work.path().join("reviews/1394662/r1");
        std::fs::create_dir_all(&review).unwrap();
        std::fs::write(review.join("review.json"), "{}").unwrap();
        let project = work.path().join("proj-x86_64");
        std::fs::create_dir_all(project.join("rpms")).unwrap();
        std::fs::write(project.join("rpms/a.rpm"), "x").unwrap();
        cleanup(&project, |_| false).unwrap();
        assert!(!project.join("rpms").exists());
        assert!(review.join("review.json").is_file());
    }

    #[test]
    fn deletes_only_rebuildable_caches_and_reports_bytes() {
        let d = layout();
        let c = cleanup(d.path(), |_| false).unwrap();
        let p = d.path();
        assert!(!p.join("rpms").exists());
        assert!(!p.join("state/pkg/sdist").exists());
        assert!(!p.join("state/pkg/sdist.stamp").exists());
        for kept in [
            "runs/r1/run.jsonl",
            "ledger.json",
            "state/pkg/state.json",
            "state/pkg/notes.md",
            "home:me:py/pkg/pkg.spec",
        ] {
            assert!(p.join(kept).exists(), "{kept}");
        }
        assert!(c.bytes >= 1501, "{}", c.bytes);
        assert_eq!(c.removed.len(), 3);
    }

    #[test]
    fn refuses_and_lists_a_dirty_checkout_deleting_nothing() {
        let d = layout();
        let err = cleanup(d.path(), |_| true).unwrap_err();
        assert!(err.contains("home:me:py"), "{err}");
        assert!(d.path().join("rpms/x86_64/a.rpm").exists());
        assert!(d.path().join("state/pkg/sdist/src/f").exists());
    }

    #[test]
    fn a_file_scheduled_for_addition_is_dirty_without_osc() {
        let d = layout();
        let wc = d.path().join("home:me:py/pkg");
        std::fs::write(wc.join(".osc/_to_be_added"), "new.patch\n").unwrap();
        assert!(checkout_is_dirty(&wc));
    }
}
