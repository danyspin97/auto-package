//! Whether the unpacked tests of a package need pytest: a `%check` that runs
//! `python -m unittest` still needs it when the test files import pytest.

use std::path::Path;

/// How far below the source root the scan goes.
const MAX_DEPTH: usize = 6;
/// How many test files the scan reads before it gives up.
const MAX_FILES: usize = 400;

/// Whether pytest may be needed by the tests under `source`: a test file
/// imports pytest or uses its API. A test file is a `.py` file under a `tests`
/// or `test` directory, or one named `test_*.py`, `*_test.py` or `conftest.py`.
/// `true` as well when the scan cannot finish (too many files, unreadable).
pub fn tests_may_need_pytest(source: &Path) -> bool {
    let Ok(uses) =
        regex::Regex::new(r"(?m)^\s*(import\s+pytest\b|from\s+pytest\b)|(^|[^\w.])pytest\.\w")
    else {
        return true;
    };
    if !source.is_dir() {
        return true;
    }
    // The root may itself be a hidden directory (a temporary one): only the
    // entries below it are skipped when hidden.
    let walk = walkdir::WalkDir::new(source)
        .max_depth(MAX_DEPTH)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
    let mut read = 0;
    for entry in walk.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".py") {
            continue;
        }
        let in_test_dir = entry
            .path()
            .strip_prefix(source)
            .ok()
            .and_then(|rel| rel.parent())
            .is_some_and(|dir| {
                dir.components().any(|c| {
                    let c = c.as_os_str().to_string_lossy();
                    c == "tests" || c == "test"
                })
            });
        let is_test_file =
            name.starts_with("test_") || name.ends_with("_test.py") || name == "conftest.py";
        if !in_test_dir && !is_test_file {
            continue;
        }
        read += 1;
        if read > MAX_FILES {
            return true;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            return true;
        };
        if uses.is_match(&String::from_utf8_lossy(&bytes)) {
            return true;
        }
    }
    false
}
