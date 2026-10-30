//! Source files stay at most 600 lines (enforced by a test).

use std::path::{Path, PathBuf};

const MAX_LINES: usize = 600;

/// Files over the limit that are not split yet, relative to the repository
/// root. The list can only shrink: an entry that is no longer over the limit
/// fails the test, so remove it when its file is split.
const ALLOWED: &[&str] = &[];

/// Every `.rs` file under `dir`, skipping `target/` and dot directories
/// (`.claude` holds agent worktrees, `.jj` and `.git` hold VCS data).
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn source_files_stay_within_the_line_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&root, &mut files);

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
