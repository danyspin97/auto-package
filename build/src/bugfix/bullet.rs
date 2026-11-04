//! The `.changes` bullet of a fix.

use std::path::Path;

fn newest_entry(changes: &str) -> Option<String> {
    let is_separator = |l: &str| {
        let l = l.trim();
        l.len() >= 20 && l.bytes().all(|b| b == b'-')
    };
    let mut lines = changes.lines().skip_while(|l| !is_separator(l)).skip(1);
    let mut entry: Vec<&str> = Vec::new();
    for l in lines.by_ref() {
        if is_separator(l) {
            break;
        }
        entry.push(l);
    }
    (!entry.is_empty()).then(|| entry.join("\n"))
}

/// What to do with the `.changes` text to carry `bullet` for `file`.
#[derive(Debug, PartialEq, Eq)]
pub enum BulletEdit {
    /// The newest entry is the baseline's: this run has no entry yet, so the
    /// bullet opens one.
    NewEntry,
    /// The newest entry is this run's: its text with the bullet that names
    /// `file` replaced by `bullet`, or `bullet` first when none names it.
    Rewritten(String),
    /// The entry already has this bullet.
    Unchanged,
}

/// How `bullet` gets into `current`, given the `.changes` text before the stage
/// (`baseline`).
pub fn fix_bullet_edit(current: &str, baseline: &str, file: &str, bullet: &str) -> BulletEdit {
    if newest_entry(current).is_none() || newest_entry(current) == newest_entry(baseline) {
        return BulletEdit::NewEntry;
    }
    match crate::backport::with_backport_bullets(current, &[(file.to_string(), bullet.to_string())])
    {
        Some(text) => BulletEdit::Rewritten(text),
        None => BulletEdit::Unchanged,
    }
}

/// Write the bullet of the fix `file` into the checkout's newest `.changes`:
/// before the first commit, so the fix is one commit and one build. `add_entry`
/// opens a new entry (`osc vc`) and gets the bullet without its `- `.
pub fn set_fix_bullet(
    checkout: &Path,
    baseline: &str,
    file: &str,
    bullet: &str,
    add_entry: &dyn Fn(&Path, &str) -> Result<(), String>,
) -> Result<(), String> {
    let path = crate::osc::newest_changes_file(checkout);
    let current = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    match fix_bullet_edit(&current, baseline, file, bullet) {
        BulletEdit::NewEntry => add_entry(checkout, bullet.trim_start_matches("- ")),
        BulletEdit::Rewritten(text) => {
            let path = path.ok_or("the package has no .changes file")?;
            ap_core::fsx::write_atomic(&path, text)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))
        }
        BulletEdit::Unchanged => Ok(()),
    }
}
