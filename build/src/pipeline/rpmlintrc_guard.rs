//! The format of an rpmlintrc an edit writes (convention 39): only `addFilter(...)`
//! lines (one may span lines), comments and blank lines, and one to three
//! `# reason: <text>` lines directly above every filter the edit adds. An edit
//! that leaves such a file in another shape is rolled back.

use super::*;
use crate::pipeline::render::*;

/// Whether `name` is an rpmlintrc file: `<name>-rpmlintrc`, or an older
/// `<name>.rpmlintrc`.
pub(super) fn is_rpmlintrc(name: &str) -> bool {
    name.ends_with("-rpmlintrc") || name.ends_with(".rpmlintrc")
}

/// Which lines of `new` the edit added: those outside a longest common
/// subsequence with `old`, so a line equal to an old line elsewhere in the file is
/// still an added line.
fn added_lines(old: &[&str], new: &[&str]) -> Vec<bool> {
    if old.len().saturating_mul(new.len()) > 4_000_000 {
        return new.iter().map(|l| !old.contains(l)).collect();
    }
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut added = vec![true; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            added[j] = false;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    added
}

/// The text of a `# reason:` line, `None` when the line is not one or has no text.
fn reason_text(line: &str) -> Option<&str> {
    line.strip_prefix("# reason:")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// What is wrong with the lines of an rpmlintrc `text` that `old` (the file as it
/// was, `None` for a new file) does not have, one entry per problem. A line the
/// file already had, at the same place, is the file's own business: the edit may
/// leave it. A filter the edit adds needs one to three `# reason: <text>` lines
/// directly above it, the nearest of them added by the edit too (not borrowed from
/// the filter above). A filter may span lines: its parentheses must balance.
pub fn rpmlintrc_problems(old: Option<&str>, text: &str) -> Vec<String> {
    let old_lines: Vec<&str> = old
        .map(|o| o.lines().map(str::trim).collect())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let added = added_lines(&old_lines, &lines);
    let mut problems = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (line, n) = (lines[i], i + 1);
        if line.is_empty() || line.starts_with('#') || !added[i] {
            i += 1;
            continue;
        }
        if !line.starts_with("addFilter(") {
            problems.push(format!(
                "line {n} (`{line}`) is not an `addFilter(...)` line, a comment or blank"
            ));
            i += 1;
            continue;
        }
        // The statement ends where its parentheses balance.
        let mut depth = 0i32;
        let mut end = i;
        for (k, l) in lines.iter().enumerate().skip(i) {
            depth += l.matches('(').count() as i32 - l.matches(')').count() as i32;
            end = k;
            if depth <= 0 {
                break;
            }
        }
        if depth > 0 {
            problems.push(format!(
                "line {n} (`{line}`) starts an `addFilter(` whose parentheses are never closed"
            ));
        }
        let reasons = (0..3)
            .take_while(|d| i > *d && reason_text(lines[i - 1 - d]).is_some())
            .count();
        if reasons == 0 {
            let empty = i > 0 && lines[i - 1].starts_with("# reason:");
            problems.push(format!(
                "line {n} (`{line}`) has no `# reason: <text>` line directly above it{}",
                if empty {
                    " (the reason line is empty)"
                } else {
                    ""
                }
            ));
        } else if !added[i - 1] {
            problems.push(format!(
                "line {n} (`{line}`) is under a `# reason:` line the file already had: \
                 write its own reason line directly above it"
            ));
        }
        i = end + 1;
    }
    problems
}

/// The acceptance check for the rpmlintrc files of an edit: each one the edit
/// added or changed must have the format above. The edit is rolled back the way
/// a rejected spec is. Returns the reason when it was.
pub(super) fn reject_bad_rpmlintrc(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
) -> Option<String> {
    let snap = snapshot?;
    let dir = spec_path.parent()?;
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_rpmlintrc(n))
        .collect();
    names.sort();
    let mut found = Vec::new();
    for name in names {
        let Ok(bytes) = std::fs::read(dir.join(&name)) else {
            continue;
        };
        if snap.holds_unchanged(&name, &bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let old = snap
            .content_of(&name)
            .map(|o| String::from_utf8_lossy(&o).into_owned());
        found.extend(
            rpmlintrc_problems(old.as_deref(), &text)
                .into_iter()
                .map(|p| format!("{name}: {p}")),
        );
    }
    if found.is_empty() {
        return None;
    }
    let problem = format!(
        "the edit adds rpmlintrc lines that convention 39 does not allow: {}. A line the edit \
         adds is an `addFilter(...)` (it may span lines) with its own `# reason: <text>` comment \
         line directly above it (no `setBadness`, no `from Config import *`); lines the file \
         already had may stay.",
        found.join("; ")
    );
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::BadRpmlintrc {
            problems: found.clone(),
        },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    snap.restore_keeping(reporter, Some(step), keep);
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — the rpmlintrc has the wrong shape\n{problem}\nThe edit was rolled back.\n"
        ),
    );
    Some(problem)
}
