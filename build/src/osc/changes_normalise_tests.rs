use super::changes_normalise::{
    normalise_file, normalise_unpublished_entries, normalise_with_notes,
};

const SEP: &str = "-------------------------------------------------------------------\n";

/// The `.changes` of Base:System/debugedit: 5.3 is published.
const ORIGIN: &str = "\
-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 5.3:
  * Support files with more than 65280 ELF sections.
- Rebase finddebuginfo.patch.
- Add BuildRequires on gcc, gcc-c++, and make.
";

/// The branch after lint rebased it onto 5.3: the new entry on top, the
/// unpublished 5.4 entry of the earlier update below it.
const DEBUGEDIT_BRANCH: &str = "\
-------------------------------------------------------------------
Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>

- Update to 5.3

-------------------------------------------------------------------
Mon Nov  2 16:38:10 UTC 2026 - Jane Doe <jane.doe@example.com>

- Update to 5.4
  * No upstream changelog found or provided
- Refresh debuginfo-mono.patch
- Refresh debuglink.patch
- Refresh debugsubpkg.patch
- Refresh finddebuginfo-absolute-links.patch
- Refresh workaround-missing-linked-file.patch
- Require libxxhash 0.8.0 or newer, as configure.ac now demands
- Add gdb and dwz BuildRequires, which configure needs for
  find-debuginfo

-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 5.3:
  * Support files with more than 65280 ELF sections.
- Rebase finddebuginfo.patch.
- Add BuildRequires on gcc, gcc-c++, and make.
";

pub(super) fn entry(header: &str, body: &str) -> String {
    format!("{SEP}{header}\n\n{body}\n")
}

pub(super) const HEADER: &str = "Mon Nov  2 16:38:10 UTC 2026 - A <a@b.c>";

#[test]
fn a_stale_entry_and_a_duplicate_of_a_published_one_are_dropped() {
    // The 5.3 entry on top duplicates the published 5.3 one, the 5.4 entry
    // is for another release: only the origin's text is left.
    let out = normalise_unpublished_entries(ORIGIN, DEBUGEDIT_BRANCH, "5.3", "debugedit").unwrap();
    assert_eq!(out, ORIGIN);
}

#[test]
fn an_entry_for_the_spec_version_loses_dependency_bullets_and_groups_refreshes() {
    let out = normalise_unpublished_entries(ORIGIN, DEBUGEDIT_BRANCH, "5.4", "debugedit").unwrap();
    let want = "\
- Update to 5.4
- Refresh patches:
  * debuginfo-mono.patch
  * debuglink.patch
  * debugsubpkg.patch
  * finddebuginfo-absolute-links.patch
  * workaround-missing-linked-file.patch

";
    assert!(out.contains(want), "{out}");
    assert!(!out.contains("libxxhash"));
    assert!(!out.contains("dwz"));
    // The published entry stays as it is, and the 5.3 entry on top duplicates
    // it, so it goes.
    assert!(out.ends_with(ORIGIN));
    assert_eq!(out.matches("Update to 5.3").count(), 1);
}

#[test]
fn a_placeholder_bullet_is_removed_from_an_update_entry() {
    let branch = entry(
        HEADER,
        "- Update to 1.1\n  * No upstream changelog found or provided\n- Fix the build\n- No release notes.",
    );
    let out = normalise_unpublished_entries("", &branch, "1.1", "pkg").unwrap();
    assert!(!out.contains("No upstream"), "{out}");
    assert!(!out.contains("No release notes"), "{out}");
    assert!(out.contains("- Update to 1.1\n- Fix the build\n"), "{out}");
}

#[test]
fn a_placeholder_is_recognised_as_a_bullet_or_a_nested_bullet_only() {
    use super::changes_normalise::is_placeholder;
    assert!(is_placeholder("  * No upstream changelog"));
    assert!(is_placeholder("- no release notes"));
    assert!(is_placeholder("No upstream changelog found or provided."));
    assert!(!is_placeholder(
        "  * No upstream changelog was read, so see the tarball"
    ));
    assert!(!is_placeholder("- Fix the build"));
}

#[test]
fn one_refresh_stays_one_line() {
    let branch = entry(HEADER, "- Update to 1.1\n- Refresh x.patch");
    assert_eq!(
        normalise_unpublished_entries("", &branch, "1.1", "pkg"),
        None
    );
}

#[test]
fn a_refresh_of_a_word_that_is_no_patch_is_not_grouped() {
    let branch = entry(HEADER, "- Update to 1.1\n- Refresh docs\n- Refresh a.patch");
    assert_eq!(
        normalise_unpublished_entries("", &branch, "1.1", "pkg"),
        None
    );
}

#[test]
fn duplicate_bullets_are_joined() {
    let body = "\
- Update to 2.10.0
- Refresh fortune-avoid-rinutils.patch
- Refresh fortune-no-games.patch
- Build from the fortune-mod subdirectory of the upstream
  repository archive
- Install the README and Notes docs from the fortune-mod
  subdirectory
- Build from the fortune-mod subdirectory of the upstream
  repository archive
- Install the README and Notes docs from the fortune-mod
  subdirectory";
    let branch = entry(HEADER, body);
    let out = normalise_unpublished_entries("", &branch, "2.10.0", "fortune").unwrap();
    assert_eq!(out.matches("Build from the fortune-mod").count(), 1);
    assert_eq!(out.matches("Install the README").count(), 1);
    assert!(out.contains(
        "- Refresh patches:\n  * fortune-avoid-rinutils.patch\n  * fortune-no-games.patch\n"
    ));
}

#[test]
fn a_published_entry_is_never_touched() {
    let published = entry(
        HEADER,
        "- Update to 1.0\n- Refresh a.patch\n- Refresh b.patch\n- Add BuildRequires on foo.",
    );
    assert_eq!(
        normalise_unpublished_entries(&published, &published, "2.0", "pkg"),
        None
    );
}

#[test]
fn an_origin_entry_edited_in_the_branch_is_kept_as_it_is() {
    let edited = format!(
        "{SEP}Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>\n\n\
         - Update to 5.3:\n  * Edited in the branch.\n"
    );
    assert_eq!(
        normalise_unpublished_entries(ORIGIN, &edited, "5.3", "debugedit"),
        None
    );
}

#[test]
fn a_non_update_entry_is_kept_whole() {
    let branch = entry(
        HEADER,
        "- Fix the build with gcc 16\n- Raise the dependency on foo",
    );
    assert_eq!(
        normalise_unpublished_entries("", &branch, "1.0", "pkg"),
        None
    );
}

#[test]
fn a_prose_only_entry_is_kept() {
    let branch = entry(HEADER, "Rebuilt for the new toolchain.");
    assert_eq!(
        normalise_unpublished_entries("", &branch, "1.0", "pkg"),
        None
    );
}

#[test]
fn an_update_to_a_snapshot_or_an_official_build_is_not_a_version() {
    let branch = entry(
        HEADER,
        "- Update to git snapshot 2026-11-01\n- Update to official 2.8.5",
    );
    assert_eq!(
        normalise_unpublished_entries("", &branch, "5.4", "pkg"),
        None
    );
}

#[test]
fn a_version_with_a_suffix_is_the_same_release() {
    let branch = entry(HEADER, "- Update to 5.4-1");
    assert_eq!(
        normalise_unpublished_entries("", &branch, "5.4", "pkg"),
        None
    );
}

#[test]
fn older_unpublished_entries_for_one_version_are_merged() {
    // The newest entry's header stays; the bullets of both follow, the older
    // one's first; the bullet they share is kept once.
    let branch = format!(
        "{}{}",
        entry(
            "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
            "- Update to 5.4\n- Refresh a.patch\n- Fix the build"
        ),
        entry(
            HEADER,
            "- Update to 5.4\n- Refresh b.patch\n- Fix the build"
        ),
    );
    let out = normalise_unpublished_entries("", &branch, "5.4", "pkg").unwrap();
    assert_eq!(out.matches("Update to 5.4").count(), 1, "{out}");
    assert_eq!(out.matches("Fix the build").count(), 1, "{out}");
    assert!(out.contains("Wed Nov  4 13:08:52"), "{out}");
    assert!(!out.contains("Mon Nov  2"), "{out}");
    assert!(
        out.contains(
            "- Update to 5.4\n- Refresh patches:\n  * b.patch\n  * a.patch\n- Fix the build\n"
        ),
        "{out}"
    );
    assert_eq!(normalise_unpublished_entries("", &out, "5.4", "pkg"), None);
}

/// The branch of debugedit after a rerun of update: the empty 5.4 entry of the
/// rerun on top, the 5.3 entry that lint rebased onto, then the first 5.4 one.
const DEBUGEDIT_RERUN: &str = "\
-------------------------------------------------------------------
Wed Nov  4 13:39:02 UTC 2026 - Jane Doe <jane.doe@example.com>

- Update to 5.4

-------------------------------------------------------------------
Wed Nov  4 13:08:52 UTC 2026 - Jane Doe <jane.doe@example.com>

- Update to 5.3

-------------------------------------------------------------------
Mon Nov  2 16:38:10 UTC 2026 - Jane Doe <jane.doe@example.com>

- Update to 5.4
  * No upstream changelog found or provided
- Refresh debuginfo-mono.patch
- Refresh debuglink.patch
- Refresh debugsubpkg.patch
- Refresh finddebuginfo-absolute-links.patch
- Refresh workaround-missing-linked-file.patch
- Require libxxhash 0.8.0 or newer, as configure.ac now demands
- Add gdb and dwz BuildRequires, which configure needs for
  find-debuginfo

-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 5.3:
  * Support files with more than 65280 ELF sections.
- Rebase finddebuginfo.patch.
- Add BuildRequires on gcc, gcc-c++, and make.
";

#[test]
fn the_rerun_entries_of_one_version_become_one_entry() {
    let out = normalise_unpublished_entries(ORIGIN, DEBUGEDIT_RERUN, "5.4", "debugedit").unwrap();
    let want = "\
-------------------------------------------------------------------
Wed Nov  4 13:39:02 UTC 2026 - Jane Doe <jane.doe@example.com>

- Update to 5.4
- Refresh patches:
  * debuginfo-mono.patch
  * debuglink.patch
  * debugsubpkg.patch
  * finddebuginfo-absolute-links.patch
  * workaround-missing-linked-file.patch

";
    assert!(out.starts_with(want), "{out}");
    assert!(out.ends_with(ORIGIN), "{out}");
    assert_eq!(out.matches("Update to 5.4").count(), 1, "{out}");
    assert!(!out.contains("Nov  4 13:08"), "{out}");
    assert!(!out.contains("libxxhash"), "{out}");
    assert!(!out.contains("dwz"), "{out}");
    assert!(!out.contains("No upstream"), "{out}");
    // Normalising the result again changes nothing.
    assert_eq!(
        normalise_unpublished_entries(ORIGIN, &out, "5.4", "debugedit"),
        None
    );
}

#[test]
fn a_crlf_branch_stays_crlf() {
    let branch = DEBUGEDIT_BRANCH.replace('\n', "\r\n");
    let out = normalise_unpublished_entries(ORIGIN, &branch, "5.4", "debugedit").unwrap();
    assert!(out.contains("- Update to 5.4\r\n"), "{out:?}");
    assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
}

#[test]
fn a_crlf_branch_that_needs_nothing_is_none() {
    let origin = ORIGIN.replace('\n', "\r\n");
    assert_eq!(
        normalise_unpublished_entries(&origin, &origin, "5.3", "debugedit"),
        None
    );
}

#[test]
fn a_macro_version_changes_nothing_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("debugedit.changes");
    std::fs::write(&file, DEBUGEDIT_BRANCH).unwrap();
    assert!(normalise_file(&file, ORIGIN, Some("%{upstream}"), "debugedit").is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), DEBUGEDIT_BRANCH);
    assert!(normalise_file(&file, ORIGIN, None, "debugedit").is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), DEBUGEDIT_BRANCH);
}

#[test]
fn a_changed_file_is_written_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("debugedit.changes");
    std::fs::write(&file, DEBUGEDIT_BRANCH).unwrap();
    assert!(!normalise_file(&file, ORIGIN, Some("5.3"), "debugedit").is_empty());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), ORIGIN);
    assert!(normalise_file(&file, ORIGIN, Some("5.3"), "debugedit").is_empty());
}

#[test]
fn the_run_reports_what_it_did() {
    let (text, notes) = normalise_with_notes(ORIGIN, DEBUGEDIT_RERUN, "5.4", "debugedit");
    assert!(text.is_some());
    assert!(notes.merged && notes.placeholder, "{notes:?}");
    assert_eq!(notes.stale_newest, None);
}

#[test]
fn a_repeated_update_bullet_keeps_its_recap_lines_once() {
    let branch = entry(
        HEADER,
        "- Update to 5.4\n  * a\n- Refresh x.patch\n- Update to 5.4:\n  * b\n  * a",
    );
    let out = normalise_unpublished_entries("", &branch, "5.4", "pkg").unwrap();
    assert!(
        out.contains("- Update to 5.4\n  * a\n  * b\n- Refresh x.patch\n"),
        "{out}"
    );
    assert_eq!(out.matches("Update to 5.4").count(), 1, "{out}");
}

#[test]
fn the_flags_and_recap_of_an_older_entry_do_not_come_back() {
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.4\n  * recap new\n  FLAG: tests skipped: y",
    );
    let older = entry(
        HEADER,
        "- Update to 5.4\n  * recap old\n  FLAG: tests skipped: x\n- Fix the build",
    );
    let out = normalise_unpublished_entries("", &format!("{newer}{older}"), "5.4", "pkg").unwrap();
    assert!(
        out.contains("- Update to 5.4\n  * recap new\n  FLAG: tests skipped: y\n- Fix the build\n"),
        "{out}"
    );
    assert!(!out.contains("recap old"), "{out}");
    assert!(!out.contains("tests skipped: x"), "{out}");
    // The newest entry has no flag now (the tests pass): an older one's does
    // not come back either.
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.4",
    );
    let out = normalise_unpublished_entries("", &format!("{newer}{older}"), "5.4", "pkg").unwrap();
    assert!(!out.contains("FLAG"), "{out}");
    assert!(out.contains("- Fix the build"), "{out}");
}

#[test]
fn a_newer_entry_that_names_another_version_too_is_still_merged() {
    // The newest entry names 5.3 as well: that older release's bullet is kept
    // under the 5.4 one, the entry stays a 5.4 one, and the older 5.4 entry is
    // not dropped with it.
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.4\n- Update to 5.3\n  * old recap\n- Fix the build",
    );
    let older = entry(HEADER, "- Update to 5.4\n- Refresh b.patch");
    let out = normalise_unpublished_entries("", &format!("{newer}{older}"), "5.4", "pkg").unwrap();
    assert!(
        out.contains(
            "- Update to 5.4\n- Update to 5.3\n  * old recap\n- Refresh b.patch\n- Fix the build\n"
        ),
        "{out}"
    );
    assert_eq!(out.matches("Update to 5.4").count(), 1, "{out}");
}

#[test]
fn the_newest_entry_is_kept_with_a_warning_when_it_is_another_release() {
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.4\n- Refresh n.patch",
    );
    let older = entry(HEADER, "- Update to 5.2\n- Refresh o.patch");
    let (text, notes) = normalise_with_notes("", &newer, "5.3", "pkg");
    assert_eq!(text, None);
    assert_eq!(notes.stale_newest.as_deref(), Some("5.4"));
    // An older release than the spec's is kept below it, and the newest one
    // stays: 5.2 is older than 5.3, so its entry is not stale. Both already
    // have their final shape, so nothing changes.
    let (text, notes) = normalise_with_notes("", &format!("{newer}{older}"), "5.3", "pkg");
    assert_eq!(text, None);
    assert_eq!(notes.stale_newest.as_deref(), Some("5.4"));
}

#[test]
fn an_rc_spelling_of_the_spec_version_is_not_stale() {
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 3.1\n- Refresh n.patch",
    );
    let older = entry(HEADER, "- Update to 3.0rc1\n- Fix the build");
    // Nothing changes: the older entry is the spec's release, spelt differently,
    // so it is kept as it is (and the newest one is kept with a warning).
    let (text, notes) = normalise_with_notes("", &format!("{newer}{older}"), "3.0~rc1", "pkg");
    assert_eq!(text, None);
    assert_eq!(notes.stale_newest.as_deref(), Some("3.1"));
}

#[test]
fn a_grouped_refresh_keeps_its_other_continuation_lines() {
    let branch = entry(
        HEADER,
        "- Update to 5.4\n- Refresh patches:\n  * a.patch\n  (rebased on 5.4)\n- Refresh b.patch",
    );
    let out = normalise_unpublished_entries("", &branch, "5.4", "pkg").unwrap();
    assert!(
        out.contains("- Refresh patches:\n  * a.patch\n  * b.patch\n  (rebased on 5.4)\n"),
        "{out}"
    );
}

#[test]
fn a_crlf_merge_stays_crlf() {
    let branch = DEBUGEDIT_RERUN.replace('\n', "\r\n");
    let out = normalise_unpublished_entries(ORIGIN, &branch, "5.4", "debugedit").unwrap();
    assert!(
        out.contains("- Update to 5.4\r\n- Refresh patches:\r\n"),
        "{out:?}"
    );
    assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
}

/// An entry with a placeholder bullet (so the rewrite changes it), then `tail`
/// after its own final newline: "" leaves one newline at the end of the file,
/// "\n" one blank line, "\n\n" two.
fn file_ending_with(tail: &str) -> String {
    let body = "- Update to 1.1\n  * No upstream changelog found or provided\n- Fix the build";
    format!("{}{tail}", entry(HEADER, body))
}

#[test]
fn the_trailing_blank_lines_of_the_file_are_kept() {
    let none = normalise_unpublished_entries("", &file_ending_with(""), "1.1", "pkg").unwrap();
    assert!(none.ends_with("- Fix the build\n"), "{none:?}");
    assert!(!none.ends_with("\n\n"), "{none:?}");

    let one = normalise_unpublished_entries("", &file_ending_with("\n"), "1.1", "pkg").unwrap();
    assert!(
        one.contains("- Update to 1.1\n- Fix the build\n"),
        "{one:?}"
    );
    assert!(one.ends_with("- Fix the build\n\n"), "{one:?}");
    assert!(!one.ends_with("\n\n\n"), "{one:?}");

    let two = normalise_unpublished_entries("", &file_ending_with("\n\n"), "1.1", "pkg").unwrap();
    assert!(two.ends_with("- Fix the build\n\n\n"), "{two:?}");
    assert!(!two.ends_with("\n\n\n\n"), "{two:?}");
}

#[test]
fn a_crlf_file_keeps_its_trailing_blank_lines_as_crlf() {
    let branch = file_ending_with("\n").replace('\n', "\r\n");
    let out = normalise_unpublished_entries("", &branch, "1.1", "pkg").unwrap();
    assert!(out.ends_with("- Fix the build\r\n\r\n"), "{out:?}");
    assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
}
