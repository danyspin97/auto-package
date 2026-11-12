use super::*;
use crate::pipeline::snapshot::CheckoutSnapshot;
use std::path::PathBuf;

/// Keeps the progress messages.
#[derive(Default)]
struct Progress(std::sync::Mutex<Vec<String>>);

impl Reporter for Progress {
    fn report(&self, e: &Event) {
        if let Event::Progress { message, .. } = e {
            self.0.lock().unwrap().push(message.clone());
        }
    }
}

/// The spec that built.
const BUILT: &str = "Name: foo\nVersion: 1.0\n# Installs into %{_bindir}\n%build\nmake\n\n\
                     %changelog\n* Mon Jan 01 2026 Someone - 1.0-0\n- first\n";

const CHANGES: &str = "Mon Jan 01 2026 - Someone\n\n- first\n";

/// A checkout holding the built spec, its `.changes`, a patch, a `_service`
/// and a source archive, with the snapshot a turn is judged against.
fn checkout(dir: &std::path::Path) -> (PathBuf, CheckoutSnapshot) {
    let spec = dir.join("foo.spec");
    std::fs::write(&spec, BUILT).unwrap();
    std::fs::write(dir.join("foo.changes"), CHANGES).unwrap();
    std::fs::write(dir.join("fix.patch"), "--- a/f\n+++ b/f\n").unwrap();
    std::fs::write(dir.join("_service"), "<services/>\n").unwrap();
    std::fs::write(dir.join("foo-1.0.tar.gz"), vec![0u8; 4096]).unwrap();
    let snapshot = CheckoutSnapshot::take(dir);
    (spec, snapshot)
}

/// What a turn leaves behind: the spec as `after` and the changes entry the
/// pipeline writes for an accepted edit.
fn turn_wrote_spec(dir: &std::path::Path, spec: &std::path::Path, after: &str) {
    std::fs::write(spec, after).unwrap();
    std::fs::write(
        dir.join("foo.changes"),
        format!("- Updated comments.\n{CHANGES}"),
    )
    .unwrap();
}

/// Discard `after` (a spec that was written over `before`) with a journal in
/// its own directory, and return the progress lines and the journal text.
fn discard(
    dir: &std::path::Path,
    snapshot: &CheckoutSnapshot,
    spec: &std::path::Path,
    before: &str,
    message: &str,
) -> (Vec<String>, String) {
    let journal = dir.join("journal.txt");
    let reporter = Progress::default();
    discard_comment_only_edit(
        snapshot,
        spec,
        before,
        None,
        journal.clone(),
        message,
        &reporter,
    );
    let lines = reporter.0.lock().unwrap().clone();
    (lines, std::fs::read_to_string(journal).unwrap_or_default())
}

#[test]
fn a_comment_only_edit_is_discarded_and_the_built_spec_stands() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    let after = BUILT.replace("# Installs into %{_bindir}", "# Installs into %%{_bindir}");
    turn_wrote_spec(dir.path(), &spec, &after);
    assert!(is_comment_only_edit(&snapshot, &spec, BUILT));

    let journal_dir = tempfile::tempdir().unwrap();
    let (lines, journal) = discard(
        journal_dir.path(),
        &snapshot,
        &spec,
        BUILT,
        REVIEW_COMMENT_ONLY,
    );
    assert_eq!(std::fs::read_to_string(&spec).unwrap(), BUILT);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("foo.changes")).unwrap(),
        CHANGES
    );
    assert_eq!(
        lines,
        vec!["the review changed only comments; the spec that built stands".to_string()]
    );
    assert!(journal.contains("Comment-only edit discarded"), "{journal}");
}

#[test]
fn the_rpmlint_fix_discard_has_its_own_progress_line() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &BUILT.replace("# Installs", "# Puts"));
    let journal_dir = tempfile::tempdir().unwrap();
    let (lines, _) = discard(
        journal_dir.path(),
        &snapshot,
        &spec,
        BUILT,
        RPMLINT_COMMENT_ONLY,
    );
    assert_eq!(
        lines,
        vec!["the rpmlint fix changed only comments; the spec that built stands".to_string()]
    );
    assert_eq!(std::fs::read_to_string(&spec).unwrap(), BUILT);
}

#[test]
fn a_crlf_spec_is_restored_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let built = "Name: foo\r\n# old\r\n%build\r\nmake\r\n";
    let spec = dir.path().join("foo.spec");
    std::fs::write(&spec, built).unwrap();
    let snapshot = CheckoutSnapshot::take(dir.path());
    std::fs::write(&spec, "Name: foo\r\n# new\r\n%build\r\nmake\r\n").unwrap();
    assert!(is_comment_only_edit(&snapshot, &spec, built));
    discard(dir.path(), &snapshot, &spec, built, REVIEW_COMMENT_ONLY);
    assert_eq!(std::fs::read(&spec).unwrap(), built.as_bytes());
}

#[test]
fn a_code_edit_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &BUILT.replace("make", "make install"));
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn a_changelog_text_edit_is_a_code_edit() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &BUILT.replace("- first", "- second"));
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn a_hash_line_added_to_the_changelog_is_a_code_edit() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(
        dir.path(),
        &spec,
        &BUILT.replace("- first", "# note\n- first"),
    );
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn a_comment_edit_with_a_code_edit_is_a_code_edit() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    let after = BUILT
        .replace("# Installs into %{_bindir}", "# Installs into %%{_bindir}")
        .replace("make", "make -j1");
    turn_wrote_spec(dir.path(), &spec, &after);
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn an_edit_that_touches_another_file_is_kept() {
    for file in ["fix.patch", "_service", "new.patch"] {
        let dir = tempfile::tempdir().unwrap();
        let (spec, snapshot) = checkout(dir.path());
        turn_wrote_spec(dir.path(), &spec, &BUILT.replace("# Installs", "# Puts"));
        std::fs::write(dir.path().join(file), "changed\n").unwrap();
        assert!(!is_comment_only_edit(&snapshot, &spec, BUILT), "{file}");
    }
}

#[test]
fn a_deleted_patch_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &BUILT.replace("# Installs", "# Puts"));
    std::fs::remove_file(dir.path().join("fix.patch")).unwrap();
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn an_unchanged_spec_is_not_a_comment_only_edit() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    std::fs::write(dir.path().join("foo.changes"), format!("- x\n{CHANGES}")).unwrap();
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn a_trailing_blank_or_final_newline_edit_is_not_a_comment_only_edit() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &format!("{BUILT}\n\n"));
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
    turn_wrote_spec(dir.path(), &spec, BUILT.trim_end());
    assert!(!is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn a_directive_edit_is_a_code_edit() {
    for line in [
        "#!BuildIgnore: foo",
        "#!ForceMultiVersion",
        "# nodebuginfo",
        "# norootforbuild",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (spec, snapshot) = checkout(dir.path());
        let after = BUILT.replace("%build\n", &format!("{line}\n%build\n"));
        turn_wrote_spec(dir.path(), &spec, &after);
        assert!(!is_comment_only_edit(&snapshot, &spec, BUILT), "{line}");
    }
}

#[test]
fn a_comment_inside_a_here_document_is_a_code_edit() {
    let dir = tempfile::tempdir().unwrap();
    let built = "Name: foo\n%install\ncat << EOF > gen.py\n#!/usr/bin/python3\nEOF\n";
    let spec = dir.path().join("foo.spec");
    std::fs::write(&spec, built).unwrap();
    let snapshot = CheckoutSnapshot::take(dir.path());
    std::fs::write(
        &spec,
        built.replace("#!/usr/bin/python3", "#!/usr/bin/python2"),
    )
    .unwrap();
    assert!(!is_comment_only_edit(&snapshot, &spec, built));
}

#[test]
fn an_untracked_archive_present_before_and_after_does_not_block_the_discard() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &BUILT.replace("# Installs", "# Puts"));
    assert!(is_comment_only_edit(&snapshot, &spec, BUILT));
}

#[test]
fn a_spec_that_is_empty_has_no_roll_back_point() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, snapshot) = checkout(dir.path());
    turn_wrote_spec(dir.path(), &spec, &BUILT.replace("# Installs", "# Puts"));
    assert!(!is_comment_only_edit(&snapshot, &spec, "  \n"));
}
