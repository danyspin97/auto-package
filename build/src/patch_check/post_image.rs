//! The line matching of the hunk proof (see [`super::present`]).
//!
//! A hunk that adds lines is proven by its post-image: the lines it keeps and
//! adds, in hunk order, with the removed lines dropped. Its added lines must be
//! one contiguous block of the file, in order, so an added line that is not next
//! to the others proves nothing. Between two lines of the post-image the file
//! may hold up to [`TOLERATED_GAP`] blank or comment-only lines, except between
//! two added lines: a comment that the new sources have between a context line
//! and the change does not break the proof, a line of code does. A hunk that only
//! removes lines is proven by its context being one run of the file, with the
//! removed lines not between its context lines.
//!
//! A removed substantive line still in the file within [`REMOVED_MARGIN`] lines
//! of the match, with the same indentation, refuses the proof.
//!
//! A comment-only added line may match a comment of the file that is nearly the
//! same (a typo fixed upstream): at most a tenth of the longer one may differ,
//! ignoring case and whitespace. An added code line matches a file line equal to
//! it once leading and trailing whitespace are ignored; a context line matches
//! exactly. A run needs an added code line, not only a comment.

use super::present::{Hunk, HunkLine, is_substantive};

/// Blank or comment-only lines the file may hold between two lines of a
/// post-image, other than between two added lines.
const TOLERATED_GAP: usize = 3;

/// File lines on each side of a matched run in which a removed substantive line
/// (with the same indentation) still refuses the proof.
const REMOVED_MARGIN: usize = 3;

/// Whether the post-image of a hunk that adds lines is in `lines` (see the
/// module doc).
///
/// Up to two leading and two trailing context lines may be left out of the run,
/// as a fuzzy hunk's edge context differs from the file; an added line never is.
/// The run must keep a substantive added code line, and no substantive removed
/// line may be near the match.
pub(super) fn added_in_file(lines: &[String], hunk: &Hunk) -> bool {
    let post: Vec<&HunkLine> = hunk
        .lines
        .iter()
        .filter(|l| !matches!(l, HunkLine::Removed(_)))
        .collect();
    let removed = substantive_removed(hunk);
    (0..=2).any(|head| {
        (0..=2).any(|tail| {
            trimmed_run(&post, head, tail).is_some_and(|run| run_in(lines, run, &removed))
        })
    })
}

/// Whether a removal-only hunk is in `lines`: its context is one run of the
/// file with no removed line inside it, and its old lines (context and removed,
/// in order) are not one run, so the removed lines are gone from between the
/// context lines. A removed line may still be elsewhere in the file.
pub(super) fn removal_in_file(lines: &[String], hunk: &Hunk) -> bool {
    let context: Vec<&str> = hunk
        .lines
        .iter()
        .filter_map(|l| match l {
            HunkLine::Context(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let old: Vec<&str> = hunk
        .lines
        .iter()
        .filter(|l| !matches!(l, HunkLine::Added(_)))
        .map(HunkLine::text)
        .collect();
    let removed = substantive_removed(hunk);
    if removed.is_empty() || !context.iter().any(|l| is_substantive(l)) {
        return false;
    }
    let placed = lines
        .windows(context.len())
        .any(|w| same_lines(w, &context) && !w.iter().any(|f| removed.contains(&f.as_str())));
    placed && !occurs(lines, &old)
}

/// The post-image of `post` with `head` and `tail` context lines cut from its
/// ends, or `None` when a cut line is not context, nothing is left, or the
/// run keeps no substantive added code line.
fn trimmed_run<'a>(
    post: &'a [&'a HunkLine],
    head: usize,
    tail: usize,
) -> Option<&'a [&'a HunkLine]> {
    let end = post.len().checked_sub(tail)?;
    let run = post.get(head..end)?;
    let cut_is_context = post[..head]
        .iter()
        .chain(&post[end..])
        .all(|l| matches!(l, HunkLine::Context(_)));
    let has_added = run
        .iter()
        .any(|l| matches!(l, HunkLine::Added(t) if is_substantive(t) && !is_comment(t)));
    (cut_is_context && has_added).then_some(run)
}

/// Whether `run` matches `lines` at some place, and no substantive removed line
/// stands within [`REMOVED_MARGIN`] lines of that match with the same indentation.
fn run_in(lines: &[String], run: &[&HunkLine], removed: &[&str]) -> bool {
    (0..lines.len()).any(|start| {
        matched_end(lines, start, run).is_some_and(|end| {
            let from = start.saturating_sub(REMOVED_MARGIN);
            let to = (end + REMOVED_MARGIN).min(lines.len());
            !lines[from..to]
                .iter()
                .any(|f| removed.iter().any(|r| f.trim_end() == r.trim_end()))
        })
    })
}

/// The end (exclusive) of the match of `run` whose first line is file line
/// `start`, or `None`. Each line after the first is found after at most
/// [`TOLERATED_GAP`] non-code lines, and right after the line before when both
/// are added lines.
fn matched_end(lines: &[String], start: usize, run: &[&HunkLine]) -> Option<usize> {
    let mut from = start;
    let mut prev: Option<&HunkLine> = None;
    for &item in run {
        let block = matches!((prev, item), (Some(HunkLine::Added(_)), HunkLine::Added(_)));
        let gap = if prev.is_none() || block {
            0
        } else {
            TOLERATED_GAP
        };
        from = next_match(lines, from, item, gap)? + 1;
        prev = Some(item);
    }
    Some(from)
}

/// The first file line from `from` on that stands for `item`, with at most `gap`
/// non-code lines before it. A line of code that does not stand for it ends the
/// search.
fn next_match(lines: &[String], from: usize, item: &HunkLine, gap: usize) -> Option<usize> {
    for at in from..lines.len().min(from + gap + 1) {
        if line_matches(&lines[at], item) {
            return Some(at);
        }
        if !is_non_code(&lines[at]) {
            return None;
        }
    }
    None
}

/// Whether file line `file` stands for the post-image line `item`: context lines
/// are equal, an added code line is equal once its leading and trailing
/// whitespace is ignored, a comment-only added line is a comment nearly the same.
fn line_matches(file: &str, item: &HunkLine) -> bool {
    match item {
        HunkLine::Added(text) if is_comment(text) => {
            is_comment(file) && similar_comments(file, text)
        }
        HunkLine::Added(text) => file.trim() == text.trim(),
        _ => file == item.text(),
    }
}

/// Whether a line is a comment only (a `#` after its indentation).
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Whether a line is blank or a comment only: a line a change may be kept apart
/// from its context by (see [`TOLERATED_GAP`]).
fn is_non_code(line: &str) -> bool {
    line.trim().is_empty() || is_comment(line)
}

/// Whether two comment lines are at least 90% alike: their edit distance, over
/// the characters that are not whitespace and in lower case, is at most a tenth
/// of the longer one. Integer arithmetic, so a tenth is exact.
fn similar_comments(file: &str, patch: &str) -> bool {
    let a = squashed(file);
    let b = squashed(patch);
    let longest = a.len().max(b.len());
    // The distance is at least the length difference: checked first, cheaply.
    a.len().abs_diff(b.len()) * 10 <= longest && distance(&a, &b) * 10 <= longest
}

/// The characters of `line` in lower case, without whitespace.
fn squashed(line: &str) -> Vec<char> {
    line.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The Levenshtein distance of two character strings, in two rows.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The removed lines of `hunk` that can prove anything.
fn substantive_removed(hunk: &Hunk) -> Vec<&str> {
    hunk.lines
        .iter()
        .filter_map(|l| match l {
            HunkLine::Removed(t) if is_substantive(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// Whether `needle` is one contiguous run of `lines`.
fn occurs(lines: &[String], needle: &[&str]) -> bool {
    !needle.is_empty() && lines.windows(needle.len()).any(|w| same_lines(w, needle))
}

fn same_lines(window: &[String], needle: &[&str]) -> bool {
    window.iter().zip(needle).all(|(f, n)| f.as_str() == *n)
}
