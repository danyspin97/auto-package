use super::changes_normalise::{normalise_file, normalise_unpublished_entries};

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
Wed Nov  4 13:08:52 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 5.3

-------------------------------------------------------------------
Mon Nov  2 16:38:10 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

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

fn entry(header: &str, body: &str) -> String {
    format!("{SEP}{header}\n\n{body}\n")
}

const HEADER: &str = "Mon Nov  2 16:38:10 UTC 2026 - A <a@b.c>";

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
fn an_older_unpublished_entry_for_the_same_version_is_a_duplicate() {
    let branch = format!(
        "{}{}",
        entry(
            "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
            "- Update to 5.4\n- Refresh a.patch"
        ),
        entry(HEADER, "- Update to 5.4\n- Refresh b.patch"),
    );
    let out = normalise_unpublished_entries("", &branch, "5.4", "pkg").unwrap();
    assert!(out.contains("a.patch"), "{out}");
    assert!(!out.contains("b.patch"), "{out}");
    assert_eq!(out.matches("Update to 5.4").count(), 1);
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
    assert!(!normalise_file(
        &file,
        ORIGIN,
        Some("%{upstream}"),
        "debugedit"
    ));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), DEBUGEDIT_BRANCH);
    assert!(!normalise_file(&file, ORIGIN, None, "debugedit"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), DEBUGEDIT_BRANCH);
}

#[test]
fn a_changed_file_is_written_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("debugedit.changes");
    std::fs::write(&file, DEBUGEDIT_BRANCH).unwrap();
    assert!(normalise_file(&file, ORIGIN, Some("5.3"), "debugedit"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), ORIGIN);
    assert!(!normalise_file(&file, ORIGIN, Some("5.3"), "debugedit"));
}
