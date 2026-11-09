//! The changes a patch edit adds to a patch the spec already had: what
//! `source_only_problems` judges. A rebase can move an added line to another
//! file, so an added line counts as old when the old patch added the same text
//! in any file.

use super::patch_rules_content::{AddedLine, PatchChanges, patch_changes};
use std::collections::HashMap;

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
        added: subtract_added(new.added, &old.added),
        removed: subtract(new.removed, &old.removed),
        deleted: subtract(new.deleted, &old.deleted),
    }
}

/// `new` without the added lines of `old`, one for one, by their text alone: an
/// added line whose text `old` added in any file is carried, not new.
fn subtract_added(new: Vec<AddedLine>, old: &[AddedLine]) -> Vec<AddedLine> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for item in old {
        *counts.entry(item.text.as_str()).or_default() += 1;
    }
    new.into_iter()
        .filter(|item| match counts.get_mut(item.text.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

/// `new` without the items of `old`, one for one.
fn subtract<T: Eq + std::hash::Hash + Clone>(new: Vec<T>, old: &[T]) -> Vec<T> {
    let mut counts: HashMap<&T, usize> = HashMap::new();
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
