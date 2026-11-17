//! Source files stay at most 600 lines (enforced by a test).

use std::path::{Path, PathBuf};

const MAX_LINES: usize = 600;

/// Files over the limit that are not split yet, relative to the repository
/// root. The list can only shrink: an entry that is no longer over the limit
/// fails the test, so remove it when its file is split.
const ALLOWED: &[&str] = &[];

/// Top-level directories that hold generated or scratch files, as the
/// `/ap-work/`, `/notes/` and `/spike/` entries of `.gitignore` name them.
/// Their contents are not project source (`ap-work/` holds vendored crates
/// from package builds), so the walk leaves them out.
const SKIPPED_TOP_LEVEL: &[&str] = &["ap-work", "notes", "spike"];

/// Whether the walk leaves out the directory `rel`, a path relative to the
/// repository root. Skipped: `target` at any depth, dot directories (`.claude`
/// holds agent worktrees, `.jj` and `.git` hold VCS data), and the
/// `SKIPPED_TOP_LEVEL` directories at the root only.
fn is_skipped(rel: &Path) -> bool {
    let Some(name) = rel.file_name().map(|n| n.to_string_lossy()) else {
        return false;
    };
    let top_level = rel.components().count() == 1;
    name == "target"
        || name.starts_with('.')
        || (top_level && SKIPPED_TOP_LEVEL.contains(&name.as_ref()))
}

/// Every `.rs` file under `dir` (inside `root`) that the walk does not skip.
fn rust_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(root).expect("walked from the root");
            if !is_skipped(rel) {
                rust_files(root, &path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn walk_skips_generated_and_vcs_directories_only() {
    for skipped in [
        "target",
        "ecosystem/target",
        ".claude",
        ".jj",
        ".git",
        "ap-work",
        "notes",
        "spike",
    ] {
        assert!(
            is_skipped(Path::new(skipped)),
            "{skipped} should be skipped"
        );
    }
    for kept in ["build", "ecosystem/src", "ecosystem/ap-work", "core/notes"] {
        assert!(!is_skipped(Path::new(kept)), "{kept} should be walked");
    }
}

#[test]
fn source_files_stay_within_the_line_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&root, &root, &mut files);

    let mut over = Vec::new();
    let mut seen_allowed = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .expect("walked from the root")
            .to_string_lossy()
            .into_owned();
        let lines = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("{rel}: {e}"))
            .lines()
            .count();
        if lines <= MAX_LINES {
            continue;
        }
        if ALLOWED.contains(&rel.as_str()) {
            seen_allowed.push(rel);
        } else {
            over.push(format!("{rel}: {lines} lines"));
        }
    }

    assert!(
        over.is_empty(),
        "source files over {MAX_LINES} lines (split them, or list them in ALLOWED with a reason):\n{}",
        over.join("\n")
    );
    let stale: Vec<&&str> = ALLOWED
        .iter()
        .filter(|a| !seen_allowed.iter().any(|s| s == *a))
        .collect();
    assert!(
        stale.is_empty(),
        "ALLOWED entries that are no longer over {MAX_LINES} lines or do not exist; remove them: {stale:?}"
    );
}
