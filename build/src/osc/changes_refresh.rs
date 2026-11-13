//! The `.changes` bullet of quilt refreshes: one `Refresh` bullet per release
//! entry, grouped when it names several patches. A run rewrites that bullet in
//! place, so the entry keeps exactly one however many runs added patches.

use super::*;
use std::path::{Path, PathBuf};

/// The newest `.changes` file of the checkout and its text.
fn newest_text(checkout_dir: &Path) -> Option<(PathBuf, String)> {
    let file = newest_changes_file(checkout_dir)?;
    let text = fs::read_to_string(&file).ok()?;
    Some((file, text))
}

/// The bounds of this release's entry in `lines`: the separator before its
/// header, and the one after it. `None` when the newest entry is not this
/// release's (it has no `Update to <version>` bullet).
fn release_entry(lines: &[&str], version: &str) -> Option<(usize, usize)> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    let is_this_release = lines[first + 1..end]
        .iter()
        .any(|l| is_release_bullet(l.trim().trim_start_matches(['-', '*', ' ']), version));
    is_this_release.then_some((first, end))
}

/// The refresh bullets in `lines[from..end]`, as `[start, end)` line ranges: a
/// `- Refresh <patch>` line, or a `- Refresh patches:` line with the indented
/// `* <patch>` lines under it.
fn refresh_ranges(lines: &[&str], from: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = from;
    while i < end {
        let Some(head) = lines[i].strip_prefix("- ") else {
            i += 1;
            continue;
        };
        if !head.starts_with("Refresh ") {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if head.trim_end() == "Refresh patches:" {
            while j < end && lines[j].starts_with(' ') && lines[j].trim().starts_with("* ") {
                j += 1;
            }
        }
        out.push((i, j));
        i = j;
    }
    out
}

/// The patches one refresh range names.
fn names_of(lines: &[&str], (start, end): (usize, usize)) -> Vec<String> {
    let head = lines[start]
        .strip_prefix("- ")
        .unwrap_or_default()
        .trim_end();
    if head == "Refresh patches:" {
        lines[start + 1..end]
            .iter()
            .filter_map(|l| l.trim().strip_prefix("* "))
            .map(str::to_string)
            .collect()
    } else {
        head.strip_prefix("Refresh ")
            .map(|n| vec![n.to_string()])
            .unwrap_or_default()
    }
}

/// The patches this release's entry already names in its refresh bullet.
/// Empty when the newest entry is not this release's.
pub fn listed_refreshes(checkout_dir: &Path, version: &str) -> Vec<String> {
    let Some((_, text)) = newest_text(checkout_dir) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some((first, end)) = release_entry(&lines, version) else {
        return Vec::new();
    };
    refresh_ranges(&lines, first + 1, end)
        .into_iter()
        .flat_map(|r| names_of(&lines, r))
        .collect()
}

/// `changes` with this release's refresh bullets replaced by `block` (`Refresh`
/// head and `  * ` items, as [`refresh_bullet`] renders them): the first bullet
/// takes the place of the first refresh bullet, the others are dropped. An
/// empty `block` removes them. `None` when the entry has no refresh bullet.
fn replace_refresh_block(changes: &str, version: &str, block: &str) -> Option<String> {
    let lines: Vec<&str> = changes.lines().collect();
    let (first, end) = release_entry(&lines, version)?;
    let ranges = refresh_ranges(&lines, first + 1, end);
    let start = ranges.first()?.0;
    let block_lines: Vec<String> = block
        .lines()
        .enumerate()
        .map(|(k, l)| {
            if k == 0 {
                format!("- {l}")
            } else {
                l.to_string()
            }
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        if k == start {
            out.extend(block_lines.iter().cloned());
        }
        if ranges.iter().any(|(i, j)| (*i..*j).contains(&k)) {
            continue;
        }
        out.push(line.to_string());
    }
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

/// Sets this release's refresh bullet to `block` (see [`replace_refresh_block`]).
/// With no refresh bullet yet, `block` is added as one bullet of the entry.
pub fn set_refresh_bullet(
    checkout_dir: &Path,
    version: &str,
    block: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    if let Some((file, text)) = newest_text(checkout_dir)
        && let Some(replaced) = replace_refresh_block(&text, version, block.trim_end())
    {
        if replaced == text {
            return Ok(());
        }
        return fs::write(&file, replaced)
            .map_err(|e| Error::NotActionable(format!("cannot write {}: {e}", file.display())));
    }
    add_release_changes_block(checkout_dir, version, block, emit)
}

/// Adds `block` as one bullet of this release's entry. `block` is a bullet head
/// and, for a group, its `  * ` items on the following lines; unlike
/// [`add_release_changes_entry`], the lines are not folded one by one.
pub fn add_release_changes_block(
    checkout_dir: &Path,
    version: &str,
    block: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let block = block.trim_end();
    if block.trim().is_empty() {
        return Ok(());
    }
    add_release_bullet(checkout_dir, version, block, emit)
}
