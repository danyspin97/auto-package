//! The date of a `.changes` entry header, as OBS's source validator reads it.
//! Its `date` call rejects a 12-hour time whose hour is 0 or above 12, as in
//! `Tue Sep 26 00:00:00 PM CEST 2023`, so `osc ci` fails with `INVALID DATE`
//! whatever the tool writes. An origin entry is never rewritten (it is
//! published): the commit step stops once, with the line.

use super::run_start_copy;
use regex::Regex;
use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;

/// The first entry header of `text` whose date `date` cannot read: its line
/// (1-based) and the line itself. A header without an AM or PM mark is read
/// as a 24-hour time and is not judged.
pub fn invalid_entry_date(text: &str) -> Option<(usize, String)> {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(r"^[A-Z][a-z]{2} [A-Z][a-z]{2} +\d{1,2} (\d{1,2}):\d{2}:\d{2} (AM|PM)\b")
            .expect("a valid pattern")
    });
    text.lines().enumerate().find_map(|(i, line)| {
        let hour: u32 = header.captures(line)?.get(1)?.as_str().parse().ok()?;
        (!(1..=12).contains(&hour)).then(|| (i + 1, line.to_string()))
    })
}

/// The reason to stop with when a `.changes` file that OBS holds (its copy
/// from the run start, see [`run_start_copy`]) has an entry header OBS cannot
/// read. `None` when every such date is valid.
pub fn origin_changes_date_problem(checkout: &Path, baseline: &Path) -> Option<String> {
    let mut names: Vec<OsString> = std::fs::read_dir(checkout)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| n.to_string_lossy().ends_with(".changes"))
        .collect();
    names.sort();
    names.iter().find_map(|name| {
        let text = run_start_copy(checkout, Some(baseline), name);
        let (line, header) = invalid_entry_date(&text)?;
        Some(format!(
            "needs a human: the origin's {} has an invalid date at line {line} ({}): OBS's \
             source validator rejects it, so every commit fails the same way; the published \
             entry is not rewritten and nothing was sent to OBS",
            name.to_string_lossy(),
            header.trim()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_header_date_passes() {
        let text = "-------------------------------------------------------------------\n\
                    Tue Sep 26 00:00:00 UTC 2023 - a <a@b.c>\n\n- Update to 1.0\n\
                    Tue Sep 26 10:00:00 PM CEST 2023 - a <a@b.c>\n\
                    Tue Sep 26 12:30:00 AM CEST 2023 - a <a@b.c>\n";
        assert_eq!(invalid_entry_date(text), None);
    }

    #[test]
    fn a_zero_hour_with_pm_is_flagged_with_its_line() {
        let text = "-----\nTue Sep 26 10:00:00 UTC 2023 - a <a@b.c>\n\n- x\n\
                    Tue Sep 26 00:00:00 PM CEST 2023 - a <a@b.c>\n";
        let (line, header) = invalid_entry_date(text).expect("flagged");
        assert_eq!(line, 5);
        assert!(header.contains("00:00:00 PM"), "{header}");
    }

    #[test]
    fn an_hour_above_twelve_with_am_is_flagged() {
        assert!(invalid_entry_date("Tue Sep 26 13:00:00 AM CEST 2023 - a\n").is_some());
    }

    #[test]
    fn the_origin_copy_is_judged_and_the_working_copy_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("pkg");
        std::fs::create_dir_all(checkout.join(".osc").join("sources")).unwrap();
        // Our file is valid; the origin's copy (kept by osc) is not.
        std::fs::write(
            checkout.join("pkg.changes"),
            "Tue Sep 26 10:00:00 UTC 2023 - a\n",
        )
        .unwrap();
        std::fs::write(
            checkout.join(".osc").join("sources").join("pkg.changes"),
            "Tue Sep 26 00:00:00 PM CEST 2023 - a\n",
        )
        .unwrap();
        let problem = origin_changes_date_problem(&checkout, &dir.path().join("none"))
            .expect("the origin's date is flagged");
        assert!(
            problem.starts_with("needs a human: the origin's pkg.changes"),
            "{problem}"
        );
        assert!(problem.contains("at line 1"), "{problem}");
        std::fs::write(
            checkout.join(".osc").join("sources").join("pkg.changes"),
            "Tue Sep 26 10:00:00 UTC 2023 - a\n",
        )
        .unwrap();
        assert_eq!(
            origin_changes_date_problem(&checkout, &dir.path().join("none")),
            None
        );
    }
}
