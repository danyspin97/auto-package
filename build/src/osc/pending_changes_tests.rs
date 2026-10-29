use super::*;
use crate::osc::changes::*;

const SEP: &str = "-------------------------------------------------------------------";

fn entry(date: &str, bullet: &str) -> String {
    format!("{SEP}\n{date} - Me <me@x.org>\n\n- {bullet}\n\n")
}

#[test]
fn the_entries_of_one_upload_become_one_entry_with_every_bullet() {
    let committed = entry("Mon", "Initial packaging");
    let current = format!(
        "{}{}{committed}",
        entry("Wed", "Relax the numpy bound"),
        entry("Tue", "Add a patch")
    );
    let merged = merge_pending_changes(&current, &committed).unwrap();
    assert_eq!(
        merged,
        format!(
            "{SEP}\nWed - Me <me@x.org>\n\n- Add a patch\n- Relax the numpy bound\n\n{committed}"
        )
    );
    // Merging again finds one new entry: nothing to do.
    assert!(merge_pending_changes(&merged, &committed).is_none());
}

#[test]
fn a_new_package_merges_against_an_empty_committed_file() {
    let current = format!(
        "{}{}",
        entry("Tue", "Fix"),
        entry("Mon", "Initial packaging")
    );
    let merged = merge_pending_changes(&current, "").unwrap();
    assert!(merged.contains("- Initial packaging\n- Fix\n"));
    assert_eq!(merged.matches(SEP).count(), 1);
}

#[test]
fn an_edited_history_is_left_alone() {
    let current = format!("{}{}", entry("Tue", "Fix"), entry("Mon", "Other"));
    assert!(merge_pending_changes(&current, &entry("Mon", "Initial")).is_none());
}

#[test]
fn the_file_is_merged_against_the_committed_copy() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".osc/sources")).unwrap();
    let committed = entry("Mon", "Initial packaging");
    fs::write(dir.path().join(".osc/sources/p.changes"), &committed).unwrap();
    let current = format!("{}{}{committed}", entry("Wed", "B"), entry("Tue", "A"));
    fs::write(dir.path().join("p.changes"), current).unwrap();
    merge_pending_changes_file(dir.path());
    let text = fs::read_to_string(dir.path().join("p.changes")).unwrap();
    assert_eq!(text.matches(SEP).count(), 2);
    assert!(text.contains("- A\n- B\n"));
}
