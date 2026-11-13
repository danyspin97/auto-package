//! The recap of an update: indented lines under the `Update to <version>`
//! bullet of the newest `.changes` entry.

use super::*;

/// Sets the recap lines of this release's `.changes` entry: each `lines` item
/// becomes an indented continuation line under the `Update to <version>`
/// bullet of the newest entry, so the bullet stays one bullet. A recap an
/// earlier pass wrote is replaced. Nothing is written when the newest entry is
/// not this release's.
pub fn set_update_notes(checkout_dir: &Path, version: &str, lines: &[String]) -> Result<(), Error> {
    set_release_notes(checkout_dir, &[(version.to_string(), lines.to_vec())])
}

/// One release of an update's recap: the version and its notes lines (`* ...`,
/// as written, without their indent).
pub type ReleaseNotes = (String, Vec<String>);

/// [`set_update_notes`] for several releases of one update, newest first: the
/// newest entry's own release (the first) is its `Update to` bullet, the older
/// ones get bullets of their own after it (see [`with_release_notes`]).
pub fn set_release_notes(checkout_dir: &Path, releases: &[ReleaseNotes]) -> Result<(), Error> {
    // A placeholder is not a note: it is never written under a bullet.
    let releases: Vec<ReleaseNotes> = releases
        .iter()
        .map(|(v, lines)| {
            let lines = lines
                .iter()
                .filter(|l| !is_placeholder(l))
                .cloned()
                .collect();
            (v.clone(), lines)
        })
        .collect();
    fs::read_dir(checkout_dir).map_err(|e| {
        Error::NotActionable(format!("cannot read {}: {e}", checkout_dir.display()))
    })?;
    let Some(file) = newest_changes_file(checkout_dir) else {
        return Ok(());
    };
    let text = fs::read_to_string(&file)
        .map_err(|e| Error::NotActionable(format!("cannot read {}: {e}", file.display())))?;
    match with_release_notes(&text, &releases) {
        Some(folded) if folded != text => fs::write(&file, folded)
            .map_err(|e| Error::NotActionable(format!("cannot write {}: {e}", file.display()))),
        _ => Ok(()),
    }
}

/// `changes` with its `Update to <version>` entry's recap set to `lines` (see
/// [`set_update_notes`]). `None` when the newest entry is not that release's.
/// One release only: the writer of a run is [`with_release_notes`].
#[cfg(test)]
pub(super) fn with_update_notes_in_release_entry(
    changes: &str,
    version: &str,
    lines: &[String],
) -> Option<String> {
    with_release_notes(changes, &[(version.to_string(), lines.to_vec())])
}

/// `changes` with the newest entry's recap set to `releases`, newest first. The
/// first release is the entry's own: its `Update to <version>` bullet (with or
/// without a colon) gets the notes. One release is written exactly as before,
/// keeping the bullet as it is. Several are written as `Update to <version>:`
/// bullets, the older ones after the own one, each with its notes; an older
/// bullet an earlier pass wrote for one of them is replaced. `None` when the
/// newest entry is not the first release's.
pub(super) fn with_release_notes(changes: &str, releases: &[ReleaseNotes]) -> Option<String> {
    let (own, own_lines) = releases.first()?;
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let all: Vec<&str> = changes.lines().collect();
    let first = all.iter().position(|l| is_separator(l.trim()))?;
    let end = all[first + 1..]
        .iter()
        .position(|l| is_separator(l.trim()))
        .map_or(all.len(), |i| first + 1 + i);
    let bullet_text = |l: &str| l.trim().trim_start_matches(['-', '*', ' ']).to_string();
    let is_bullet_of = |l: &str, v: &str| {
        !l.starts_with(' ') && {
            let text = bullet_text(l);
            text == format!("Update to {v}") || text == format!("Update to {v}:")
        }
    };
    let at = first
        + 1
        + all[first + 1..end]
            .iter()
            .position(|l| is_bullet_of(l, own))?;
    // The bullet's block runs to the next bullet of the entry (column 0).
    let block_end = all[at + 1..end]
        .iter()
        .position(|l| !l.starts_with(' ') && !l.trim().is_empty())
        .map_or(end, |i| at + 1 + i);
    // What stays under the bullet: the flag lines and the blank lines that
    // close the entry. Every other indented line is an earlier recap.
    let block = &all[at + 1..block_end];
    let flag = flag_line_mask(block);
    let mut kept: Vec<String> = (0..block.len())
        .filter(|&i| !block[i].starts_with(' ') || flag[i])
        .map(|i| block[i].to_string())
        .collect();
    let closing = kept.split_off(
        kept.iter()
            .rposition(|l| !l.trim().is_empty())
            .map_or(0, |i| i + 1),
    );
    let mut out: Vec<String> = all[..=at].iter().map(|l| l.to_string()).collect();
    if releases.len() > 1 {
        out.pop();
        out.push(format!("- Update to {own}:"));
    }
    out.extend(own_lines.iter().map(|l| format!("  {l}")));
    out.extend(kept);
    for (v, lines) in &releases[1..] {
        out.push(format!("- Update to {v}:"));
        out.extend(lines.iter().map(|l| format!("  {l}")));
    }
    out.extend(closing);
    // The rest of the entry, without the older bullets an earlier pass wrote
    // for these releases; the rest of the file after it.
    let mut dropping = false;
    for l in &all[block_end..end] {
        if !l.starts_with(' ') && !l.trim().is_empty() {
            dropping = releases[1..].iter().any(|(v, _)| is_bullet_of(l, v));
        }
        if !dropping {
            out.push(l.to_string());
        }
    }
    out.extend(all[end..].iter().map(|l| l.to_string()));
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
    fn a_wrapped_flag_stays_whole_and_a_bullet_with_a_flag_word_is_not_a_flag() {
        let flag = "  FLAG: tests skipped: aaaa bbbb\n    cccc dddd";
        let working = entry(&format!(
            "- Update to 2.0\n{flag}\n- {} FLAG: marker\n  more words\n",
            "x".repeat(62)
        ));
        let out = with_update_notes_in_release_entry(&working, "2.0", &lines(&["* a"])).unwrap();
        assert!(out.contains(&format!("  * a\n{flag}\n- ")), "{out}");
        assert!(out.contains("FLAG: marker\n  more words"), "{out}");
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
    fn a_placeholder_recap_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pkg.changes");
        let working = entry("- Update to 2.0\n");
        std::fs::write(&file, &working).unwrap();
        let recap = lines(&["  * No upstream changelog found or provided"]);
        set_update_notes(dir.path(), "2.0", &recap).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), working);
    }

    #[test]
    fn another_releases_entry_is_never_written_to() {
        let working = entry("- Update to 2.0\n");
        assert!(with_update_notes_in_release_entry(&working, "1.0", &lines(&["* a"])).is_none());
        assert!(with_update_notes_in_release_entry(&working, "3.0", &lines(&["* a"])).is_none());
    }

    #[test]
    fn the_bullet_is_found_with_or_without_a_colon() {
        let with = entry("- Update to 2.0:\n- Fix the build\n");
        let out = with_update_notes_in_release_entry(&with, "2.0", &lines(&["* a"])).unwrap();
        assert!(
            out.contains("- Update to 2.0:\n  * a\n- Fix the build\n"),
            "{out}"
        );
    }

    #[test]
    fn several_releases_get_one_bullet_each_newest_first() {
        let working = entry("- Update to 2.0\n- Fix the build\n");
        let releases = vec![
            ("2.0".to_string(), lines(&["* a"])),
            ("1.9".to_string(), lines(&["* b"])),
        ];
        let out = with_release_notes(&working, &releases).unwrap();
        assert!(
            out.contains("- Update to 2.0:\n  * a\n- Update to 1.9:\n  * b\n- Fix the build\n"),
            "{out}"
        );
        assert_eq!(with_release_notes(&out, &releases).unwrap(), out);
    }

    #[test]
    fn an_older_release_bullet_from_an_earlier_pass_is_replaced() {
        let working =
            entry("- Update to 2.0:\n  * a\n- Update to 1.9:\n  * stale\n- Fix the build\n");
        let releases = vec![
            ("2.0".to_string(), lines(&["* a"])),
            ("1.9".to_string(), lines(&["* b"])),
        ];
        let out = with_release_notes(&working, &releases).unwrap();
        assert!(!out.contains("stale"), "{out}");
        assert_eq!(out.matches("Update to 1.9").count(), 1, "{out}");
        assert!(
            out.contains("- Update to 1.9:\n  * b\n- Fix the build\n"),
            "{out}"
        );
    }

    #[test]
    fn one_release_is_written_as_today() {
        let working = entry("- Update to 2.0\n- Fix the build\n");
        let releases = vec![("2.0".to_string(), lines(&["* a"]))];
        let out = with_release_notes(&working, &releases).unwrap();
        assert!(
            out.contains("- Update to 2.0\n  * a\n- Fix the build\n"),
            "{out}"
        );
        assert!(!out.contains("Update to 2.0:"), "{out}");
    }
}
