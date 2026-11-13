//! Taking back a bullet of the release's own `.changes` entry: a later fix
//! that replaces an earlier one makes the earlier bullet untrue.

use super::*;
use crate::osc::changes::{is_changes_separator, newest_changes_file};

/// The bullets of the newest entry of `changes`, each as one line of text
/// (continuation lines joined by a space), when that entry is the
/// `Update to <version>` entry of this release. `None` otherwise.
pub fn release_entry_bullets(changes: &str, version: &str) -> Option<Vec<String>> {
    let (_, blocks) = release_blocks(changes, version)?;
    Some(blocks.into_iter().map(|(_, _, text)| text).collect())
}

/// The newest entry's bullets as `(first line, end line, text)` with line
/// indexes into `changes.lines()`, plus the lines. `None` unless the entry
/// has an `Update to <version>` bullet.
type Blocks<'a> = (Vec<&'a str>, Vec<(usize, usize, String)>);

fn release_blocks<'a>(changes: &'a str, version: &str) -> Option<Blocks<'a>> {
    let lines: Vec<&str> = changes.lines().collect();
    let first = lines.iter().position(|l| is_changes_separator(l))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_changes_separator(l))
        .map_or(lines.len(), |i| first + 1 + i);
    let is_bullet = |l: &str| l.starts_with("- ") || l.starts_with("* ");
    let mut blocks: Vec<(usize, usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(first + 1) {
        if is_bullet(line) {
            blocks.push((i, i + 1, line[2..].trim().to_string()));
        } else if let Some(last) = blocks.last_mut()
            && !line.trim().is_empty()
        {
            last.1 = i + 1;
            last.2 = format!("{} {}", last.2, line.trim());
        }
    }
    blocks
        .iter()
        .any(|b| is_release_bullet(&b.2, version))
        .then_some((lines, blocks))
}

fn squash(text: &str) -> String {
    let text = text.trim().trim_start_matches(['-', '*', ' ']);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `changes` without the bullets of the release entry that say one of `drops`
/// (whitespace aside; the bullet's continuation lines go with it). The
/// `Update to <version>` bullet stays. `None` when nothing was dropped or the
/// newest entry is not this release's.
pub(super) fn without_release_bullets(
    changes: &str,
    version: &str,
    drops: &[String],
) -> Option<String> {
    let (lines, blocks) = release_blocks(changes, version)?;
    let wanted: Vec<String> = drops.iter().map(|d| squash(d)).collect();
    let mut gone = vec![false; lines.len()];
    let mut any = false;
    for (from, to, text) in &blocks {
        if !is_release_bullet(text, version) && wanted.iter().any(|w| w == &squash(text)) {
            gone[*from..*to].iter_mut().for_each(|g| *g = true);
            any = true;
        }
    }
    if !any {
        return None;
    }
    let mut out = lines
        .iter()
        .zip(&gone)
        .filter(|(_, g)| !**g)
        .map(|(l, _)| *l)
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    Some(out)
}

/// [`without_release_bullets`] on the checkout's `.changes`. Best effort: a
/// file that cannot be read or written is left alone.
pub fn drop_release_bullets(checkout_dir: &Path, version: &str, drops: &[String]) {
    if drops.is_empty() {
        return;
    }
    if let Some(file) = newest_changes_file(checkout_dir)
        && let Ok(text) = fs::read_to_string(&file)
        && let Some(cleaned) = without_release_bullets(&text, version, drops)
    {
        let _ = fs::write(&file, cleaned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEP: &str = "-------------------------------------------------------------------";

    fn file() -> String {
        format!(
            "{SEP}\nThu Nov  5 17:05:23 UTC 2026 - A <a@b.c>\n\n- Update to 26.11\n\
             - Import the build root while\n  install.py runs\n- Keep this\n\n\
             {SEP}\nOld header\n\n- Import the build root while\n  install.py runs\n"
        )
    }

    #[test]
    fn a_dropped_bullet_goes_with_its_continuation_lines_in_the_release_entry_only() {
        let out = without_release_bullets(
            &file(),
            "26.11",
            &["Import the build root while install.py runs".into()],
        )
        .unwrap();
        assert!(out.contains("- Update to 26.11\n- Keep this\n"));
        assert!(!out.contains("install.py runs\n- Keep"));
        // The older entry keeps its own bullet.
        assert!(out.ends_with("- Import the build root while\n  install.py runs\n"));
    }

    #[test]
    fn the_update_bullet_and_unknown_bullets_stay() {
        assert!(without_release_bullets(&file(), "26.11", &["Update to 26.11".into()]).is_none());
        assert!(without_release_bullets(&file(), "26.11", &["Other".into()]).is_none());
        assert!(without_release_bullets(&file(), "27.1", &["Keep this".into()]).is_none());
    }

    #[test]
    fn the_bullets_of_the_release_entry_are_listed() {
        assert_eq!(
            release_entry_bullets(&file(), "26.11").unwrap(),
            vec![
                "Update to 26.11",
                "Import the build root while install.py runs",
                "Keep this"
            ]
        );
        assert!(release_entry_bullets(&file(), "1.0").is_none());
    }
}
