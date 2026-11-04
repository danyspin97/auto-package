//! The source-only rule of the patch rules (see `patch_rules`): what the changes a
//! patch makes may do to tests, to the build system and to error handling. Only
//! the lines a patch adds or removes are read, so a patch that is changed is
//! judged on what the change adds, not on the whole of the old patch.

use super::*;

/// Test markers that only test code uses (an attribute or a macro): a removed line
/// with one of these removes a test in any file, unless an added line of the same
/// kind comes back for it (a rename or a fix of its name).
const TEST_MARKERS: &[&str] = &[
    "#[test]",
    "#[tokio::test]",
    "#[cfg(test)]",
    "BOOST_AUTO_TEST_CASE",
    "TEST(",
    "TEST_F(",
    "TEST_P(",
    "TEST_CASE(",
];

/// Test markers other code uses too (`it(`, `def test_`): a removed line with one
/// of these removes a test only in a test file, so a helper named like one in a
/// source file is not refused.
const TEST_FILE_MARKERS: &[&str] = &[
    "def test_",
    "func Test",
    "@Test",
    "class Test",
    "describe(",
    "it(",
];

/// Lines that skip a test when added to a test source.
const SKIP_MARKERS: &[&str] = &[
    "@pytest.mark.skip",
    "pytest.skip(",
    "@unittest.skip",
    "#[ignore]",
    "GTEST_SKIP()",
    "t.Skip(",
];

/// Autoconf checks: a line that starts with one says what a check found
/// (`AC_CHECK_LIB([m], [cos], , [have_m=no])`), not that a feature is switched off.
const AUTOCONF_CHECKS: &[&str] = &[
    "AC_CHECK",
    "AC_MSG_",
    "AC_SEARCH_LIBS",
    "AM_CHECK",
    "AM_PATH_",
];

/// Build-system files: a change to one of them may not switch off a test or check.
pub(super) const BUILD_FILES: &[&str] = &[
    "CMakeLists.txt",
    "meson.build",
    "meson_options.txt",
    "meson.options",
    "configure",
    "configure.ac",
    "configure.in",
    "Makefile",
    "GNUmakefile",
    "SConstruct",
    "setup.py",
    "setup.cfg",
    "pyproject.toml",
    "wscript",
    "BUILD",
    "BUILD.bazel",
];

/// Words of a flag name that name a test (`BUILD_TESTING`, `WITH_TESTS`).
const TEST_WORDS: &[&str] = &["test", "tests", "testing", "testsuite"];

/// Words of a flag name that name a skip: `SKIP_TESTS=ON`, `--disable-tests`.
const SKIP_NAME_WORDS: &[&str] = &[
    "skip", "skips", "skipped", "disable", "disabled", "exclude", "excluded", "ignore", "ignored",
];

/// Words of a flag name that enable a feature or name a build option (`ENABLE_X`,
/// `WITH_X`, `BUILD_X`).
const ENABLE_WORDS: &[&str] = &["enable", "enabled", "with", "build"];

/// Values that set a flag on, and values that set it off.
const ON_WORDS: &[&str] = &["on", "true", "yes", "1"];
const OFF_WORDS: &[&str] = &["off", "false", "no", "0"];

/// What a removed build-system line holds when it starts or enables a test or check.
const TEST_STEP_MARKERS: &[&str] = &[
    "add_test(",
    "enable_testing(",
    "add_subdirectory(test",
    "subdir('test",
    "subdir(\"test",
    "subdir(test",
];

/// One added line of a patch: its file, its text, and whether the line or the line
/// above it carries a comment, the explanation a `-Wno-error` or a skip flag needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct AddedLine {
    pub(super) file: String,
    pub(super) text: String,
    pub(super) explained: bool,
}

/// What a patch changes: each added and removed line with the file it is in, and the
/// files its diff deletes or renames away.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct PatchChanges {
    pub(super) added: Vec<AddedLine>,
    pub(super) removed: Vec<(String, String)>,
    pub(super) deleted: Vec<String>,
}

/// The file name a `---`/`+++` line gives, without the timestamp after a tab.
fn diff_path(rest: &str) -> &str {
    rest.split('\t').next().unwrap_or("").trim()
}

/// The `(old, new)` line counts of a `@@ -a,b +c,d @@` hunk header.
fn hunk_counts(line: &str) -> (usize, usize) {
    let mut parts = line.split_whitespace().skip(1).map(|p| {
        let digits = p.trim_start_matches(['-', '+']).split(',').nth(1);
        digits.and_then(|d| d.parse().ok()).unwrap_or(1)
    });
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Splits a patch into its changed lines, file by file. A hunk's lines are read by
/// its counts, so a removed `-- x` followed by an added `++ y` is not a file header.
pub(super) fn patch_changes(text: &str) -> PatchChanges {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = PatchChanges::default();
    let mut file = String::new();
    // The last line of the new file above the current one, in the current hunk.
    let mut above = String::new();
    // Lines of the current hunk still to read, old side and new side.
    let (mut old_left, mut new_left) = (0usize, 0usize);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if old_left > 0 || new_left > 0 {
            if let Some(added) = line.strip_prefix('+') {
                new_left = new_left.saturating_sub(1);
                let explained = has_comment(added) || has_comment(&above);
                out.added.push(AddedLine {
                    file: file.clone(),
                    text: added.to_string(),
                    explained,
                });
                above = added.to_string();
            } else if let Some(removed) = line.strip_prefix('-') {
                old_left = old_left.saturating_sub(1);
                out.removed.push((file.clone(), removed.to_string()));
            } else if !line.starts_with('\\') {
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
                above = line.strip_prefix(' ').unwrap_or(line).to_string();
            }
            continue;
        }
        if line.starts_with("@@") {
            (old_left, new_left) = hunk_counts(line);
            above.clear();
        } else if let (Some(old), Some(new)) = (
            line.strip_prefix("--- "),
            lines.get(i).and_then(|n| n.strip_prefix("+++ ")),
        ) {
            let (old, new) = (diff_path(old), diff_path(new));
            if new == "/dev/null" {
                out.deleted.push(old.to_string());
                file = old.to_string();
            } else {
                file = new.to_string();
            }
            above.clear();
            i += 1;
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            file = rest
                .split_whitespace()
                .last()
                .unwrap_or("")
                .trim_start_matches("b/")
                .to_string();
        } else if let Some(old) = line.strip_prefix("rename from ") {
            out.deleted.push(old.trim().to_string());
        } else if let Some(new) = line.strip_prefix("rename to ") {
            file = new.trim().to_string();
        } else if line.starts_with("deleted file mode") {
            out.deleted.push(file.clone());
        }
    }
    out
}

/// The changes of a patch that the edit did not already have: `new` minus `old`,
/// as lines (a file, a line and a deletion are counted as many times as they occur).
/// With no `old` every change is new.
pub(crate) fn patch_delta(old: Option<&str>, new: &str) -> PatchChanges {
    let new = patch_changes(new);
    let Some(old) = old else {
        return new;
    };
    let old = patch_changes(old);
    PatchChanges {
        added: subtract(new.added, &old.added),
        removed: subtract(new.removed, &old.removed),
        deleted: subtract(new.deleted, &old.deleted),
    }
}

/// `new` without the items of `old`, one for one.
fn subtract<T: Eq + std::hash::Hash + Clone>(new: Vec<T>, old: &[T]) -> Vec<T> {
    let mut counts: std::collections::HashMap<&T, usize> = std::collections::HashMap::new();
    for item in old {
        *counts.entry(item).or_default() += 1;
    }
    new.into_iter()
        .filter(|item| match counts.get_mut(item) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

/// Whether `path` is a test file or sits in a test directory.
pub(super) fn is_test_path(path: &str) -> bool {
    let p = Path::new(path);
    let in_test_dir = p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(
                "test"
                    | "tests"
                    | "testing"
                    | "testsuite"
                    | "t"
                    | "__tests__"
                    | "spec"
                    | "unittests"
            )
        )
    });
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = name.split('.').next().unwrap_or("");
    in_test_dir
        || name == "conftest.py"
        || name.contains(".test.")
        || name.contains(".spec.")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.ends_with("Test")
        || stem.ends_with("Tests")
}

/// Whether `path` is a build-system file.
pub(super) fn is_build_file(path: &str) -> bool {
    let p = Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    BUILD_FILES.contains(&name)
        || name.starts_with("Makefile")
        || matches!(ext, "cmake" | "mk" | "am" | "ac")
}

/// Whether a line carries a comment, which is where a `-Wno-error` says why.
fn has_comment(line: &str) -> bool {
    line.contains('#') || line.contains("//") || line.contains("/*")
}

/// Whether a line is a comment in a build or source file.
fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    ["#", "//", "/*", "*", "!"].iter().any(|p| t.starts_with(p))
}

/// The byte offset of the first place `marker` occurs in `line` as a word: a marker
/// that starts with a letter is not found inside a longer name (`it(` is not in
/// `split(`).
fn find_marker(line: &str, marker: &str) -> Option<usize> {
    let word_start = marker.starts_with(|c: char| c.is_alphanumeric() || c == '_');
    line.match_indices(marker).map(|(i, _)| i).find(|&i| {
        !word_start
            || line[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// The test marker a line of `file` holds, with the name that follows it (empty for a
/// bare attribute such as `#[test]`). The [`TEST_FILE_MARKERS`] count in a test file only.
fn test_marker(file: &str, line: &str) -> Option<(&'static str, String)> {
    let in_test_file = is_test_path(file);
    let mut markers = TEST_MARKERS
        .iter()
        .chain(TEST_FILE_MARKERS.iter().filter(|_| in_test_file));
    markers.find_map(|m| {
        let at = find_marker(line, m)? + m.len();
        let name: String = line[at..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        Some((*m, name))
    })
}

/// Whether `line` opens a make target of a test or a check: `check:`, `test:`,
/// `check-am:`, `test-suite:`. Not an assignment (`check := ...`), not indented.
fn is_test_target(line: &str) -> bool {
    if line.starts_with(char::is_whitespace) {
        return false;
    }
    let Some((name, rest)) = line.split_once(':') else {
        return false;
    };
    let word = !name.is_empty() && !name.contains(char::is_whitespace) && !rest.starts_with('=');
    word && (["check", "test", "tests"].contains(&name)
        || name.starts_with("check-")
        || name.starts_with("test-"))
}

/// Whether a removed build-system line holds a test or check step.
fn is_test_step(line: &str) -> bool {
    let t = line.trim();
    TEST_STEP_MARKERS.iter().any(|m| t.contains(m))
        || t.starts_with("test(")
        || is_test_target(line)
}

/// Whether an added line of a build-system file switches a skip on, or a test off.
/// A comment or an autoconf check says nothing about the build, so neither is read.
/// Otherwise the words of the flag names are read, with the values within the next
/// four words: a skip word set to on (`SKIP_TESTS=ON`, `value: true`); an enable,
/// with or build word set to off (`-DWITH_GUI=OFF`, `option(BUILD_X "..." OFF)`); a
/// test word set to off (`BUILD_TESTING OFF`, `-Dtests=false`); and `--disable-X` or
/// `--without-X`. A skip word in a flag is not a test switch set to off (`SKIP_TESTS=OFF`).
fn skip_flag_line(line: &str) -> bool {
    let t = line.trim_start();
    if is_comment(line) || AUTOCONF_CHECKS.iter().any(|p| t.starts_with(p)) {
        return false;
    }
    let tokens: Vec<String> = line
        .replace("-D", " ")
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let has = |tok: &str, set: &[&str]| {
        tok.split(['_', '-'])
            .filter(|w| !w.is_empty())
            .any(|w| set.contains(&w))
    };
    let near = |i: usize, set: &[&str]| {
        tokens[i + 1..]
            .iter()
            .take(4)
            .any(|t| set.contains(&t.as_str()))
    };
    (0..tokens.len()).any(|i| {
        let tok = tokens[i].as_str();
        if tok.starts_with("--disable-") || tok.starts_with("--without-") {
            return true;
        }
        let skip = has(tok, SKIP_NAME_WORDS);
        let test = !skip && has(tok, TEST_WORDS);
        let enable = !skip && has(tok, ENABLE_WORDS);
        (skip && near(i, ON_WORDS)) || ((test || enable) && near(i, OFF_WORDS))
    })
}

/// Whether a line is shell-like text, where a command can hide its failure: a
/// build-system file, a shell script, a `.in` or `.spec` file, or a make recipe line
/// (one that starts with a tab). In C, Rust or Python `||` and `;` are not commands.
fn is_shell_text(file: &str, line: &str) -> bool {
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    line.starts_with('\t') || is_build_file(file) || matches!(ext, "sh" | "bash" | "in" | "spec")
}

/// Whether a line of `file` hides a failing command: `|| true`, `|| :`, `|| exit 0`,
/// `; true` or `set +e`. Only shell-like text is read (see [`is_shell_text`]). Each
/// word must end its command (the end of the line, or `;`, `&`, `)` or a `#` comment after it), so
/// `x = 1; true_count++` and `offset + element_size` do not match.
fn hides_failure(file: &str, line: &str) -> bool {
    if !is_shell_text(file, line) {
        return false;
    }
    let ends_command = |rest: &str| {
        let r = rest.trim_start();
        r.is_empty() || r.starts_with([';', '&', ')', '#'])
    };
    let hides_after = |rest: &str| {
        let rest = rest.trim_start();
        let word = |w: &str| rest.strip_prefix(w).is_some_and(ends_command);
        let exit_0 = rest
            .strip_prefix("exit")
            .and_then(|r| r.trim_start().strip_prefix('0'))
            .is_some_and(ends_command);
        word(":") || word("true") || exit_0
    };
    let or_hides = line
        .match_indices("||")
        .any(|(at, _)| hides_after(&line[at + 2..]));
    let semicolon_hides = line.match_indices(';').any(|(at, _)| {
        let rest = line[at + 1..].trim_start();
        rest.strip_prefix("true").is_some_and(ends_command)
    });
    let set_e = line.match_indices("set").any(|(at, _)| {
        let word_start = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '+' || c == '-'));
        word_start
            && line[at + 3..]
                .trim_start()
                .strip_prefix("+e")
                .is_some_and(ends_command)
    });
    or_hides || semicolon_hides || set_e
}

/// The directory a `add_subdirectory(dir)` or a `subdir(dir)` line adds, if it adds one.
fn subdir_of(line: &str) -> Option<String> {
    let t = line.trim();
    let arg = t
        .strip_prefix("add_subdirectory(")
        .or_else(|| t.strip_prefix("subdir("))?;
    let dir: String = arg
        .trim_start()
        .trim_start_matches(['\'', '"'])
        .chars()
        .take_while(|c| !matches!(c, ')' | '\'' | '"' | ' ' | ',' | '\t'))
        .collect();
    (!dir.is_empty()).then_some(dir)
}

/// A make or automake list assignment whose name holds `SUBDIR`, `TEST` or `CHECK`
/// (`SUBDIRS = src tests`, `TESTS += a.sh`): the name and the words the list holds.
fn list_assignment(line: &str) -> Option<(String, Vec<String>)> {
    let (name, value) = line.trim().split_once('=')?;
    let name = name.trim().trim_end_matches([':', '+', '?']).trim();
    let listed = ["SUBDIR", "TEST", "CHECK"].iter().any(|k| name.contains(k));
    let plain = name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !listed || !plain {
        return None;
    }
    let words = value
        .split_whitespace()
        .map(|w| w.trim_end_matches('\\'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    Some((name.to_string(), words))
}

/// Whether an added line of `file` adds the directory `dir` again.
fn re_added_subdir(changes: &PatchChanges, file: &str, dir: &str) -> bool {
    changes
        .added
        .iter()
        .any(|a| a.file == file && subdir_of(&a.text).as_deref() == Some(dir))
}

/// The Levenshtein distance of two names: the edits that turn one into the other.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let subst = prev[j] + usize::from(ca != *cb);
            cur.push(subst.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The problems of a patch's changes that the source-only rule refuses.
pub(crate) fn source_only_problems(changes: &PatchChanges) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |msg: String| {
        if !out.contains(&msg) {
            out.push(msg);
        }
    };
    for added in &changes.added {
        let file = &added.file;
        let text = added.text.trim();
        if hides_failure(file, &added.text) {
            add("adds `|| true` or another form that hides a failing command: fix the failure instead".to_string());
        }
        if added.text.contains("-Wno-error") && !added.explained {
            add(format!(
                "adds -Wno-error in {file} without a comment on that line or the line above saying why"
            ));
        }
        if is_test_path(file) && SKIP_MARKERS.iter().any(|m| find_marker(text, m).is_some()) {
            add(format!("adds a skip to the test {file}: {text}"));
        }
        if is_build_file(file) {
            if added.text.starts_with('\t') && text.starts_with('-') {
                add(format!(
                    "adds a make recipe in {file} that ignores its failure: {text}"
                ));
            }
            if skip_flag_line(&added.text) && !added.explained {
                add(format!(
                    "adds a skip flag in {file}: {text}; a build-system change may not skip the \
                     broken part of the build: fix it, or add a comment on that line or the line \
                     above explaining why the feature is disabled"
                ));
            }
        }
    }
    for file in &changes.deleted {
        if is_test_path(file) {
            add(format!("deletes the test file {file}"));
        }
    }
    // Each removed test marker needs an added one of the same kind, in the same file or
    // with a name within three edits (a fix of the name). Each added marker answers one
    // removed marker, so one added `#[test]` does not excuse several removed ones.
    let mut added_tests: Vec<(&str, &'static str, String)> = changes
        .added
        .iter()
        .filter_map(|a| {
            let (marker, name) = test_marker(&a.file, &a.text)?;
            Some((a.file.as_str(), marker, name))
        })
        .collect();
    for (file, line) in &changes.removed {
        let Some((marker, name)) = test_marker(file, line) else {
            continue;
        };
        let answer = added_tests.iter().position(|(f, m, n)| {
            *m == marker && (*f == file.as_str() || edit_distance(n, &name) <= 3)
        });
        match answer {
            Some(i) => {
                added_tests.remove(i);
            }
            None => add(format!("removes a test: {}", line.trim())),
        }
    }
    for (file, line) in &changes.removed {
        if !is_build_file(file) {
            continue;
        }
        if is_test_step(line) {
            add(format!(
                "removes a test or check step of {file}: {}",
                line.trim()
            ));
        } else if let Some(dir) = subdir_of(line)
            && !re_added_subdir(changes, file, &dir)
        {
            add(format!(
                "removes the subdirectory {dir} of {file}: a build change may not drop a \
                 directory that is not added back: {}",
                line.trim()
            ));
        } else if let Some((var, words)) = list_assignment(line) {
            let now = changes
                .added
                .iter()
                .filter(|a| a.file == *file)
                .find_map(|a| list_assignment(&a.text).filter(|(v, _)| *v == var));
            if let Some((_, now)) = now {
                let testing = var.contains("TEST") || var.contains("CHECK");
                if testing && now.is_empty() && !words.is_empty() {
                    add(format!(
                        "empties {var} in {file}: a build change may not drop its tests"
                    ));
                }
                for w in words
                    .iter()
                    .filter(|w| !now.contains(w) && (w.contains("test") || w.contains("check")))
                {
                    add(format!("drops the test or check {w} from {var} in {file}"));
                }
            }
        }
    }
    out
}
