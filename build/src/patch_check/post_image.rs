//! The line matching of the hunk proof (see [`super::present`]).
//!
//! A hunk that adds lines is proven by its post-image: the lines it keeps and
//! adds, in hunk order, with the removed lines dropped. That run must be one
//! contiguous run of the file, so an added line that is not next to the others
//! proves nothing, and the context between two change groups is part of the run.
//! A hunk that only removes lines is proven by its context being one run of the
//! file, with the removed lines not between its context lines.

use super::present::{Hunk, HunkLine, is_substantive};

/// Whether the post-image of a hunk that adds lines is one run of `lines`.
///
/// Up to two leading and two trailing context lines may be left out of the run,
/// as a fuzzy hunk's edge context differs from the file; an added line never is.
/// The run must keep a substantive added line, and no removed line may sit in
/// the matched lines.
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
/// run keeps no substantive added line.
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
        .any(|l| matches!(l, HunkLine::Added(t) if is_substantive(t)));
    (cut_is_context && has_added).then_some(run)
}

/// Whether `run` is one contiguous run of `lines` whose matched lines hold no
/// removed substantive line.
fn run_in(lines: &[String], run: &[&HunkLine], removed: &[&str]) -> bool {
    lines.windows(run.len()).any(|w| {
        w.iter().zip(run).all(|(f, item)| f.as_str() == item.text())
            && !w.iter().any(|f| removed.contains(&f.as_str()))
    })
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
