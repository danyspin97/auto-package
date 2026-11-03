//! The recap of an update: indented lines under the `Update to <version>`
//! bullet of the newest `.changes` entry.

use super::*;

/// Sets the recap lines of this release's `.changes` entry: each `lines` item
/// becomes an indented continuation line under the `Update to <version>`
/// bullet of the newest entry, so the bullet stays one bullet. A recap an
/// earlier pass wrote is replaced. Nothing is written when the newest entry is
/// not this release's.
pub fn set_update_notes(checkout_dir: &Path, version: &str, lines: &[String]) -> Result<(), Error> {
    fs::read_dir(checkout_dir).map_err(|e| {
        Error::NotActionable(format!("cannot read {}: {e}", checkout_dir.display()))
    })?;
    let Some(file) = newest_changes_file(checkout_dir) else {
        return Ok(());
    };
    let text = fs::read_to_string(&file)
        .map_err(|e| Error::NotActionable(format!("cannot read {}: {e}", file.display())))?;
    match with_update_notes_in_release_entry(&text, version, lines) {
        Some(folded) if folded != text => fs::write(&file, folded)
            .map_err(|e| Error::NotActionable(format!("cannot write {}: {e}", file.display()))),
        _ => Ok(()),
    }
}

/// `changes` with its `Update to <version>` entry's recap set to `lines` (see
/// [`set_update_notes`]). `None` when the newest entry is not that release's.
pub(super) fn with_update_notes_in_release_entry(
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
    let at = first
        + 1
        + all[first + 1..end]
            .iter()
            .position(|l| !l.starts_with(' ') && bullet_text(l) == update)?;
    // The bullet's block runs to the next bullet of the entry (column 0).
    let block_end = all[at + 1..end]
        .iter()
        .position(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .map_or(end, |i| at + 1 + i);
    // What stays under the bullet: the flag lines and the blank lines that
    // close the entry. Every other indented line is an earlier recap.
    let kept = all[at + 1..block_end]
        .iter()
        .filter(|l| !l.starts_with(' ') || l.trim_start().starts_with("FLAG: "));
    let mut out: Vec<String> = all[..=at].iter().map(|l| l.to_string()).collect();
    out.extend(lines.iter().map(|l| format!("  {l}")));
    out.extend(kept.map(|l| l.to_string()));
    out.extend(all[block_end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    text.push('\n');
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEP: &str = "-------------------------------------------------------------------";

    fn entry(body: &str) -> String {
        let old = format!("{SEP}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.0\n");
        format!("{SEP}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n{body}\n{old}")
    }

    fn lines(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_recap_goes_under_the_update_bullet_before_flags_and_bullets() {
        let working = entry("- Update to 2.0\n  FLAG: tests skipped\n- Fix the build\n");
        let recap = lines(&["* Add a thing", "* Fix a bug", "  over two lines"]);
        let out = with_update_notes_in_release_entry(&working, "2.0", &recap).unwrap();
        assert!(
            out.contains(
                "- Update to 2.0\n  * Add a thing\n  * Fix a bug\n    over two lines\n  FLAG: tests skipped\n- Fix the build\n"
            ),
            "{out}"
        );
        assert!(out.ends_with("- Update to 1.0\n"));
    }

    #[test]
    fn a_second_recap_replaces_the_first_and_an_empty_one_removes_it() {
        let working = entry("- Update to 2.0\n- Fix the build\n");
        let once = with_update_notes_in_release_entry(&working, "2.0", &lines(&["* a"])).unwrap();
        let twice = with_update_notes_in_release_entry(&once, "2.0", &lines(&["* b"])).unwrap();
        assert!(
            twice.contains("- Update to 2.0\n  * b\n- Fix the build\n"),
            "{twice}"
        );
        assert!(!twice.contains("* a"));
        assert_eq!(
            with_update_notes_in_release_entry(&twice, "2.0", &[]).unwrap(),
            working
        );
    }

    #[test]
    fn another_releases_entry_is_never_written_to() {
        let working = entry("- Update to 2.0\n");
        assert!(with_update_notes_in_release_entry(&working, "1.0", &lines(&["* a"])).is_none());
        assert!(with_update_notes_in_release_entry(&working, "3.0", &lines(&["* a"])).is_none());
    }
}
