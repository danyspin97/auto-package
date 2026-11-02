//! The .changes file of a package: creating it, adding and merging entries,
//! the flag lines, and the wrapping of unpublished entries.

use super::*;
use crate::osc::publish::*;

/// Create an initial `.changes` entry for a new package using `osc vc`.
///
/// Only runs when the `.changes` file does not yet exist. `osc vc` reads the
/// committer identity from `~/.config/osc/oscrc` and writes a datestamped
/// entry at the top of the file.
pub fn create_initial_changes(
    checkout_dir: &Path,
    pkg_name: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let changes_name = format!("{pkg_name}.changes");
    if checkout_dir.join(&changes_name).exists() {
        return Ok(());
    }
    add_changes_entry(checkout_dir, "Initial packaging", emit)
}

/// Add a `.changes` entry saying `message`, with `osc vc`: the committer
/// identity comes from the osc configuration, and a missing `.changes` file is
/// created. A version bump records its own entry this way, so a package
/// updated by the tool does not reach OBS (or a submit request) without one.
/// Idempotent: when the newest entry already says `message` (an earlier run
/// added it), nothing is added.
pub fn add_changes_entry(checkout_dir: &Path, message: &str, emit: Emit<'_>) -> Result<(), Error> {
    if let Ok(rd) = fs::read_dir(checkout_dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "changes"))
            .collect();
        files.sort();
        // A version given up on (a wrong upstream, later corrected) left its
        // entry at the top of the file; it was never published, so it goes.
        if let Some(file) = files.first()
            && let Some(name) = file.file_name()
            && let (Ok(working), Ok(pristine)) = (
                fs::read_to_string(file),
                fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name)),
            )
            && let Some(cleaned) = without_unpublished_update_entry(&working, &pristine, message)
        {
            let _ = fs::write(file, cleaned);
        }
        if let Some(text) = files.first().and_then(|f| fs::read_to_string(f).ok())
            && newest_changes_entry_says(&text, message)
        {
            return Ok(());
        }
    }
    let out = proc::capture("osc", &["vc", "-m", message], checkout_dir, emit)
        .map_err(during(format!("vc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("vc -m {message:?} in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    merge_pending_changes_file(checkout_dir);
    Ok(())
}

/// [`add_changes_entry`] for a change made while packaging `version`: when the
/// newest entry is this release's own `- Update to <version>` entry, the bullet
/// joins it instead of opening an entry of its own, so a package that took
/// several rounds of fixes still reads as one update. Any other newest entry
/// (an earlier release, a new package) gets a fresh entry as before.
pub fn add_release_changes_entry(
    checkout_dir: &Path,
    version: &str,
    message: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    // One bullet per line of `message`: each is folded into the entry in turn.
    for bullet in message.lines().map(str::trim).filter(|l| !l.is_empty()) {
        add_release_bullet(checkout_dir, version, bullet, emit)?;
    }
    Ok(())
}

/// One bullet of [`add_release_changes_entry`].
fn add_release_bullet(
    checkout_dir: &Path,
    version: &str,
    message: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    if let Ok(rd) = fs::read_dir(checkout_dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "changes"))
            .collect();
        files.sort();
        if let Some(file) = files.first()
            && let Ok(text) = fs::read_to_string(file)
            && let Some(folded) = with_bullet_in_release_entry(&text, version, message)
        {
            return fs::write(file, folded).map_err(|e| {
                Error::NotActionable(format!("cannot write {}: {e}", file.display()))
            });
        }
    }
    add_changes_entry(checkout_dir, message, emit)
}

/// Sets the flag lines of this release's `.changes` entry: each `lines` item
/// becomes an indented continuation line under the `Update to <version>` bullet
/// of the newest entry, so the bullet stays one bullet. Flag lines an earlier
/// pass wrote are replaced, so the entry says what the package is now. When
/// the newest entry is not this release's, each line is added as a bullet of
/// its own, unless the file already has it.
pub fn set_flag_lines(
    checkout_dir: &Path,
    version: &str,
    lines: &[String],
    emit: Emit<'_>,
) -> Result<(), Error> {
    let file = newest_changes_file(checkout_dir);
    if let Some(file) = &file
        && let Ok(text) = fs::read_to_string(file)
        && let Some(folded) = with_flag_lines_in_release_entry(&text, version, lines)
    {
        if folded != text {
            return fs::write(file, folded).map_err(|e| {
                Error::NotActionable(format!("cannot write {}: {e}", file.display()))
            });
        }
        return Ok(());
    }
    let present = file
        .as_ref()
        .and_then(|f| fs::read_to_string(f).ok())
        .unwrap_or_default();
    for line in lines {
        if !present.lines().any(|l| l.trim() == format!("- {line}")) {
            add_changes_entry(checkout_dir, line, emit)?;
        }
    }
    Ok(())
}

/// The first `.changes` file of `checkout_dir`, in name order.
pub(super) fn newest_changes_file(checkout_dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(checkout_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
        .collect();
    files.sort();
    files.into_iter().next()
}

/// `changes` with its `Update to <version>` entry's flag lines set to `lines`
/// (see [`set_flag_lines`]). `None` when the newest entry is not that release's.
pub(super) fn with_flag_lines_in_release_entry(
    changes: &str,
    version: &str,
    lines: &[String],
) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let all: Vec<&str> = changes.lines().collect();
    let first = all.iter().position(|l| is_separator(l.trim()))?;
    let end = all[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(all.len(), |i| first + 1 + i);
    let bullet_text = |l: &str| l.trim().trim_start_matches(['-', '*', ' ']).to_string();
    let update = format!("Update to {version}");
    if !all[first + 1..end]
        .iter()
        .map(|l| bullet_text(l))
        .any(|t| t == update)
    {
        return None;
    }
    // A continuation line of an earlier flag set: two spaces, then `FLAG: `.
    let stale = |l: &str| l.starts_with("  ") && l.trim_start().starts_with("FLAG: ");
    let kept: Vec<&str> = all[first + 1..end]
        .iter()
        .copied()
        .filter(|l| !stale(l))
        .collect();
    // The flags go under the `Update to` bullet: after its last line, before
    // the next bullet of the entry (a bullet starts at column 0).
    let is_bullet = |l: &str| !l.starts_with(' ') && (l.starts_with("- ") || l.starts_with("* "));
    let at = kept
        .iter()
        .position(|l| bullet_text(l) == update)
        .unwrap_or(0);
    let block_end = kept[at + 1..]
        .iter()
        .position(|l| is_bullet(l))
        .map_or(kept.len(), |i| at + 1 + i);
    let last = kept[..block_end]
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut out: Vec<String> = all[..=first].iter().map(|l| l.to_string()).collect();
    out.extend(kept[..last].iter().map(|l| l.to_string()));
    out.extend(lines.iter().map(|l| format!("  {l}")));
    out.extend(kept[last..].iter().map(|l| l.to_string()));
    out.extend(all[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

/// `changes` with `- message` added to the newest entry, when that entry says
/// `Update to <version>`. `None` otherwise; unchanged text when the entry
/// already has the bullet.
pub(super) fn with_bullet_in_release_entry(
    changes: &str,
    version: &str,
    message: &str,
) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = changes.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    let entry = &lines[first + 1..end];
    let bullet_text = |l: &&str| l.trim().trim_start_matches(['-', '*', ' ']).to_string();
    let update = format!("Update to {version}");
    if !entry.iter().map(bullet_text).any(|t| t == update) {
        return None;
    }
    if entry.iter().map(bullet_text).any(|t| t == message) {
        return Some(changes.to_string());
    }
    let last = lines[first + 1..end]
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(first, |i| first + 1 + i);
    let mut out: Vec<String> = lines[..=last].iter().map(|l| l.to_string()).collect();
    out.push(format!("- {message}"));
    out.extend(lines[last + 1..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

fn is_changes_separator(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// One bullet list per upload: the entries `osc vc` added to `current` since
/// `committed` (the `.changes` OBS has) become the newest entry alone, headed by
/// its header, with the bullets of all of them, oldest first. `None` when there
/// is nothing to merge: fewer than two new entries, or `current` does not end
/// with the committed text (somebody edited the old entries).
pub(super) fn merge_pending_changes(current: &str, committed: &str) -> Option<String> {
    let new = current.strip_suffix(committed)?;
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in new.lines() {
        if is_changes_separator(line) {
            blocks.push((String::new(), Vec::new()));
        } else if let Some((header, body)) = blocks.last_mut() {
            if header.is_empty() {
                *header = line.to_string();
            } else {
                body.push(line);
            }
        } else if !line.trim().is_empty() {
            return None;
        }
    }
    if blocks.len() < 2 {
        return None;
    }
    let mut bullets: Vec<String> = Vec::new();
    for (_, body) in blocks.iter().rev() {
        let text = body.join("\n");
        let text = text.trim_matches('\n').trim_end();
        if !text.is_empty() && !bullets.iter().any(|b| b == text) {
            bullets.push(text.to_string());
        }
    }
    let mut out = format!(
        "{}\n{}\n\n{}\n\n",
        "-".repeat(67),
        blocks[0].0,
        bullets.join("\n")
    );
    out.push_str(committed);
    Some(out)
}

/// [`merge_pending_changes`] on the checkout's `.changes`, against the copy OBS
/// has (`.osc/sources/`; none for a file not committed yet). Best effort: a
/// file that cannot be read or written is left as `osc vc` made it.
pub(super) fn merge_pending_changes_file(checkout_dir: &Path) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
        .collect();
    files.sort();
    let Some(file) = files.first() else {
        return;
    };
    let Ok(current) = fs::read_to_string(file) else {
        return;
    };
    let committed = file
        .file_name()
        .and_then(|n| fs::read_to_string(checkout_dir.join(".osc/sources").join(n)).ok())
        .unwrap_or_default();
    // No copy of what OBS holds although OBS tracks the file: every entry would
    // look new and the history would be merged into one.
    if committed.is_empty()
        && file
            .file_name()
            .is_some_and(|n| tracked_files(checkout_dir).contains(&n.to_string_lossy().to_string()))
    {
        return;
    }
    if let Some(merged) = merge_pending_changes(&current, &committed) {
        let _ = fs::write(file, merged);
    }
}

/// `working` without its newest entry, when that entry is not in `pristine`
/// (what OBS holds) and records an `- Update to <other version>` that is not
/// the `message` about to be recorded. `None` when there is nothing to drop.
pub(super) fn without_unpublished_update_entry(
    working: &str,
    pristine: &str,
    message: &str,
) -> Option<String> {
    if !message.starts_with("Update to ") {
        return None;
    }
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let next = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map(|i| first + 1 + i)?;
    let entry = lines[first..next].join("\n");
    let says_other_update = lines[first..next].iter().any(|l| {
        let t = l.trim().trim_start_matches(['-', '*', ' ']);
        t.starts_with("Update to ") && t != message
    });
    if !says_other_update || pristine.contains(entry.trim_end()) {
        return None;
    }
    let mut out = lines[..first].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&lines[next..].join("\n"));
    out.push('\n');
    Some(out)
}

/// Longest line of a `.changes` entry the style checks accept.
const CHANGES_WIDTH: usize = 67;

/// A long `- Did this and did that` bullet as two bullets, when what follows
/// the ` and ` starts with a verb of its own (a second change, not the second
/// half of one object: "Drop a and b" stays whole).
fn split_bullet(line: &str) -> Option<[String; 2]> {
    const VERBS: &[&str] = &[
        "add", "drop", "remove", "fix", "use", "switch", "enable", "disable", "install", "move",
        "replace", "require", "build", "ship", "split", "rename", "update", "set", "pass", "patch",
        "keep", "stop", "skip", "make", "list", "own", "package", "provide",
    ];
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let text = rest.strip_prefix("- ")?;
    let (first, second) = text.split_once(" and ")?;
    let (verb, _) = second.split_once(' ')?;
    if !VERBS.contains(&verb.to_ascii_lowercase().as_str()) || first.split_whitespace().count() < 2
    {
        return None;
    }
    let mut cap = second.chars();
    let second: String = cap.next()?.to_uppercase().chain(cap).collect();
    let first = first.trim_end_matches(',');
    let pad = &line[..lead];
    Some([format!("{pad}- {first}"), format!("{pad}- {second}")])
}

/// `line` broken at word boundaries into lines of at most [`CHANGES_WIDTH`]
/// characters, each continuation indented to sit under the text of a bullet
/// (`- text`, `  * text`) or under the line it continues.
pub(super) fn wrap_changes_line(line: &str) -> Vec<String> {
    if line.chars().count() <= CHANGES_WIDTH {
        return vec![line.to_string()];
    }
    if let Some(parts) = split_bullet(line) {
        return parts.iter().flat_map(|p| wrap_changes_line(p)).collect();
    }
    let lead = line.len() - line.trim_start().len();
    let rest = &line[lead..];
    let bullet = ["- ", "* "]
        .iter()
        .find(|b| rest.starts_with(**b))
        .map_or(0, |b| b.len());
    let first_prefix = &line[..lead + bullet];
    let indent = " ".repeat(lead + bullet);
    let mut out = Vec::new();
    let mut current = first_prefix.to_string();
    let mut empty = true;
    for word in rest[bullet..].split_whitespace() {
        let add = if empty { 0 } else { 1 } + word.chars().count();
        if !empty && current.chars().count() + add > CHANGES_WIDTH {
            out.push(std::mem::replace(&mut current, indent.clone()));
            empty = true;
        }
        if !empty {
            current.push(' ');
        }
        current.push_str(word);
        empty = false;
    }
    out.push(current);
    out
}

/// `working` with the lines of its newest entry wrapped, or `None` when that
/// entry is already short enough or is one OBS already holds (`pristine`):
/// a published entry is never rewritten.
pub(super) fn wrapped_newest_entry(working: &str, pristine: &str) -> Option<String> {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let lines: Vec<&str> = working.lines().collect();
    let first = lines.iter().position(|l| is_separator(l.trim()))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(lines.len(), |i| first + 1 + i);
    // The header line (date - author) is the one after the separator.
    let body_from = (first + 2).min(end);
    if !lines[body_from..end]
        .iter()
        .any(|l| l.chars().count() > CHANGES_WIDTH)
    {
        return None;
    }
    let entry = lines[first..end].join("\n");
    if pristine.contains(entry.trim_end()) {
        return None;
    }
    let mut out: Vec<String> = lines[..body_from].iter().map(|l| l.to_string()).collect();
    for l in &lines[body_from..end] {
        out.extend(wrap_changes_line(l));
    }
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

/// Wrap the long lines of the `.changes` entry this run wrote (one OBS does
/// not hold yet) so the package passes the changelog style checks.
pub fn wrap_unpublished_changes(checkout_dir: &Path) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    for file in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
    {
        let Some(name) = file.file_name() else {
            continue;
        };
        let pristine = fs::read_to_string(checkout_dir.join(".osc").join("sources").join(name))
            .unwrap_or_default();
        if let Ok(working) = fs::read_to_string(&file)
            && let Some(wrapped) = wrapped_newest_entry(&working, &pristine)
        {
            let _ = fs::write(&file, wrapped);
        }
    }
}

/// Whether the newest entry of a `.changes` file (the text between its first
/// two separator lines) has `message` as one of its lines, bullet or not.
pub(super) fn newest_changes_entry_says(changes: &str, message: &str) -> bool {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let mut started = false;
    for line in changes.lines().map(str::trim) {
        if is_separator(line) {
            if started {
                return false;
            }
            started = true;
        } else if started && line.trim_start_matches(['-', '*', ' ']) == message {
            return true;
        }
    }
    false
}
