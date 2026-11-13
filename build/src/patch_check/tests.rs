use super::*;

const PUSH_CLEAN: &str =
    "Applying patch good.patch\npatching file f.txt\n\nNow at patch good.patch\n";
const PUSH_OFFSET: &str = "Applying patch off.patch\npatching file f.txt\n\
        Hunk #1 succeeded at 5 (offset 2 lines).\n\nNow at patch off.patch\n";
const PUSH_FUZZ: &str = "Applying patch fuzz.patch\npatching file f.txt\n\
        Hunk #1 succeeded at 5 with fuzz 1 (offset -1 lines).\n\nNow at patch fuzz.patch\n";
const PUSH_FAIL: &str = "Applying patch bad.patch\npatching file g.txt\nHunk #1 FAILED at 1.\n\
        1 out of 1 hunk FAILED -- rejects in file g.txt\n\
        Patch bad.patch does not apply (enforce with -f)\n";
const PUSH_FORCED: &str = "Applying patch bad.patch\npatching file g.txt\nHunk #1 FAILED at 1.\n\
        1 out of 1 hunk FAILED -- saving rejects to file g.txt.rej\n\
        Applied patch bad.patch (forced; needs refresh)\n";
const PUSH_REVERSED: &str = "Applying patch up.patch\npatching file f.txt\nHunk #1 FAILED at 1.\n\
        1 out of 1 hunk FAILED -- rejects in file f.txt\nPatch up.patch can be reverse-applied\n";

#[test]
fn quilt_output_is_classified() {
    assert_eq!(classify_push(true, PUSH_CLEAN), PushVerdict::Clean);
    assert_eq!(
        classify_push(true, PUSH_OFFSET),
        PushVerdict::OffsetOrFuzz { fuzz: false }
    );
    assert_eq!(
        classify_push(true, PUSH_FUZZ),
        PushVerdict::OffsetOrFuzz { fuzz: true }
    );
    assert_eq!(classify_push(false, PUSH_FAIL), PushVerdict::Failed);
    // A failed push is a failure whatever its text says.
    assert_eq!(classify_push(false, PUSH_CLEAN), PushVerdict::Failed);
    assert_eq!(
        classify_push(false, PUSH_REVERSED),
        PushVerdict::ReverseApplied
    );
    // Only a failed push can be a proof.
    assert_eq!(classify_push(true, PUSH_REVERSED), PushVerdict::Clean);
}

#[test]
fn reject_files_are_read_from_both_forms() {
    assert_eq!(reject_files(PUSH_FAIL), ["g.txt.rej"]);
    assert_eq!(reject_files(PUSH_FORCED), ["g.txt.rej"]);
    assert!(reject_files(PUSH_CLEAN).is_empty());
}

#[test]
fn the_series_follows_the_patch_numbers_and_levels() {
    let spec = "Name: foo\nVersion: 2.0\nPatch10: late.patch\nPatch1: %{name}-%{version}-early.patch\n\
                    Patch2: zero.patch\nPatch3: three.patch\nPatch4: %{weird}.patch\n\
                    %prep\n%autosetup -p1 -N\n%autopatch -p1 -M 1\n%patch -P 2 -p0\n%patch3 -p3\n";
    let plan = plan_series(spec);
    let checked: Vec<SeriesEntry> = plan
        .iter()
        .filter_map(|p| match p {
            Planned::Check(e) => Some(e.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        checked.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["foo-2.0-early.patch", "zero.patch", "late.patch"]
    );
    assert_eq!(
        series_text(&checked),
        "foo-2.0-early.patch\nzero.patch -p0\nlate.patch\n"
    );
    let skipped: Vec<(&str, &str)> = plan
        .iter()
        .filter_map(|p| match p {
            Planned::Skip { name, reason } => Some((name.as_str(), reason.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert!(
        skipped
            .iter()
            .any(|(n, r)| *n == "three.patch" && r.contains("-p3"))
    );
    assert!(skipped.iter().any(|(_, r)| r.contains("plain file name")));
}

#[test]
fn a_patch_the_spec_never_applies_is_not_checked() {
    let spec =
        "Name: foo\nVersion: 1\nPatch0: a.patch\nPatch1: b.patch\n%prep\n%setup -q\n%patch1 -p1\n";
    let plan = plan_series(spec);
    assert!(matches!(&plan[0], Planned::Check(e) if e.name == "b.patch"));
    assert!(matches!(&plan[1], Planned::Skip { name, .. } if name == "a.patch"));
}

fn result(name: &str, status: PatchStatus) -> PatchResult {
    PatchResult {
        name: name.into(),
        level: 1,
        status,
        refreshed: None,
    }
}

#[test]
fn the_agent_report_names_every_patch_with_what_to_do() {
    let report = CheckReport {
        patches: vec![
            result("good.patch", PatchStatus::Applies),
            result(
                "fuzz.patch",
                PatchStatus::AppliesWithOffsetOrFuzz {
                    fuzz: true,
                    output: PUSH_FUZZ.into(),
                },
            ),
            result(
                "bad.patch",
                PatchStatus::Fails {
                    output: PUSH_FAIL.into(),
                    rejects: "--- g.txt.rej\n@@ -1,3 +1,3 @@\n-zwei\n+TWO\n".into(),
                },
            ),
            result(
                "odd.patch",
                PatchStatus::NotChecked {
                    reason: "it applies with -p2".into(),
                },
            ),
            result(
                "up.patch",
                PatchStatus::AlreadyApplied {
                    output: PUSH_REVERSED.into(),
                },
            ),
        ],
        not_run: None,
    };
    let text = agent_report(
        &report,
        "2.0",
        &[("bad.patch".into(), "foo-1.0".into())],
        Path::new("/co/pkg"),
    );
    assert!(text.contains("2.0 sources"), "{text}");
    assert!(text.contains("good.patch: applies cleanly"), "{text}");
    assert!(text.contains("fuzz.patch: applies with fuzz"), "{text}");
    assert!(text.contains("bad.patch: FAILS"), "{text}");
    assert!(text.contains("+TWO"), "{text}");
    assert!(text.contains("`foo-1.0`, another release's tree"), "{text}");
    assert!(!text.contains("candidate for removal"), "{text}");
    assert!(
        text.contains("never removed because it does not apply"),
        "{text}"
    );
    assert!(
        text.contains("up.patch: already applied upstream"),
        "{text}"
    );
    assert!(
        !text.contains("patch -"),
        "the agent is never told about `patch`: {text}"
    );
    assert!(
        text.contains("odd.patch: not checked (it applies with -p2)"),
        "{text}"
    );
    assert!(text.contains("/co/pkg"), "{text}");
    assert_eq!(
        report.tally(),
        Tally {
            failing: 1,
            refresh: 1,
            upstream: 1
        }
    );
    assert!(!report.is_clean());
    let summary = report.summary();
    assert!(
        summary.contains("1 of 5")
            && summary.contains("odd.patch")
            && summary.contains("1 already applied upstream"),
        "{summary}"
    );
}

#[test]
fn a_check_that_did_not_run_says_so_and_claims_nothing() {
    let spec = "Name: foo\nVersion: 1\nPatch0: a.patch\n%prep\n%autosetup -p1\n";
    let dir = tempfile::tempdir().unwrap();
    let report = check(spec, dir.path(), None, Refresh::No);
    assert!(report.not_run.is_some());
    assert!(matches!(
        report.patches[0].status,
        PatchStatus::NotChecked { .. }
    ));
    // Nothing failed, but the summary does not call that a success.
    assert!(
        report.summary().contains("not checked"),
        "{}",
        report.summary()
    );
}

#[test]
fn the_loop_stops_on_clean_no_progress_or_rounds() {
    let t = |failing, refresh| Tally {
        failing,
        refresh,
        upstream: 0,
    };
    assert_eq!(next_round(1, 4, t(2, 0), t(0, 0), true), NextRound::Clean);
    assert_eq!(next_round(1, 4, t(2, 0), t(1, 0), false), NextRound::Again);
    assert_eq!(next_round(1, 4, t(1, 1), t(1, 0), true), NextRound::Again);
    assert_eq!(
        next_round(1, 4, t(1, 0), t(1, 0), false),
        NextRound::NoProgress
    );
    // More failures than before and no patch changed is no progress either.
    assert_eq!(
        next_round(2, 4, t(1, 0), t(2, 0), false),
        NextRound::NoProgress
    );
    assert_eq!(
        next_round(4, 4, t(2, 0), t(1, 0), false),
        NextRound::OutOfRounds
    );
}

#[test]
fn bullets_are_one_line_per_patch_without_the_version() {
    assert_eq!(refresh_bullet(&["fix-foo.patch"]), "Refresh fix-foo.patch");
    assert_eq!(
        refresh_bullet(&["fix-foo.patch", "fix-bar.patch"]),
        "Refresh patches:\n  * fix-foo.patch\n  * fix-bar.patch"
    );
    assert_eq!(refresh_bullet(&[]), "");
    assert_eq!(
        proven_drop_bullet("fix-bar.patch", DropProof::ReverseApplies),
        "Drop fix-bar.patch: already applied upstream"
    );
    assert_eq!(
        proven_drop_bullet("fix-bar.patch", DropProof::InTarball),
        "Drop fix-bar.patch: already applied upstream"
    );
    assert_eq!(
        proven_drop_bullet("fix-bar.patch", DropProof::TargetsAbsent),
        "Drop fix-bar.patch: the file it patches is gone from the new tarball"
    );
    assert_eq!(
        proven_drop_bullet("fix-bar.patch", DropProof::Mixed),
        "Drop fix-bar.patch: already applied upstream or its files are gone"
    );
}

#[test]
fn a_turn_s_patch_changes_are_refreshes_and_removals() {
    let map = |items: &[(&str, Option<&str>)]| {
        items
            .iter()
            .map(|(n, c)| (n.to_string(), c.map(|c| c.as_bytes().to_vec())))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before = map(&[
        ("same.patch", Some("a")),
        ("refreshed.patch", Some("b")),
        ("unlisted.patch", Some("c")),
        ("deleted.patch", Some("d")),
        ("missing.patch", None),
    ]);
    let after_names: Vec<String> = [
        "same.patch",
        "refreshed.patch",
        "deleted.patch",
        "missing.patch",
    ]
    .map(String::from)
    .to_vec();
    let after = map(&[
        ("same.patch", Some("a")),
        ("refreshed.patch", Some("B")),
        ("unlisted.patch", Some("c")),
        ("deleted.patch", None),
        ("missing.patch", None),
    ]);
    let (refreshed, removed) = diff_patches(&before, &after_names, &after);
    assert_eq!(refreshed, ["refreshed.patch"]);
    assert_eq!(removed, ["deleted.patch", "unlisted.patch"]);
}

#[test]
fn truncate_cuts_on_a_char_boundary() {
    let t = truncate(&"é".repeat(10), 5);
    assert!(t.starts_with("éé") && t.contains("more bytes cut"), "{t}");
    assert_eq!(truncate("short\n", 100), "short");
}

/// A tarball `name-1.0.tar.gz` holding `files` under `name-1.0/`.
pub(super) fn tarball(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    let path = dir.join("foo-1.0.tar.gz");
    let file = std::fs::File::create(&path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for (name, body) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, format!("foo-1.0/{name}"), body.as_bytes())
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
    path
}

#[test]
fn real_quilt_classifies_good_fuzzy_and_bad_patches() {
    if !quilt_available() {
        eprintln!("quilt is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tar = tarball(
        dir.path(),
        &[
            ("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\n"),
            ("g.txt", "one\ntwo\nthree\n"),
            ("h.txt", "x\nY\nz\n"),
        ],
    );
    // Its change is already in the new sources.
    std::fs::write(
        dir.path().join("up.patch"),
        "--- a/h.txt\n+++ b/h.txt\n@@ -1,3 +1,3 @@\n x\n-y\n+Y\n z\n",
    )
    .unwrap();
    std::fs::write(
            dir.path().join("good.patch"),
            "PATCH-FIX-UPSTREAM good.patch gh#1\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n",
        )
        .unwrap();
    std::fs::write(
        dir.path().join("bad.patch"),
        "--- a/g.txt\n+++ b/g.txt\n@@ -1,3 +1,3 @@\n one\n-zwei\n+TWO\n three\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("fuzz.patch"),
        "Keep this header\n--- a/f.txt\n+++ b/f.txt\n@@ -6,5 +6,5 @@\n X\n f\n-g\n+G\n h\n Y\n",
    )
    .unwrap();
    let spec = "Name: foo\nVersion: 1.0\nPatch0: good.patch\nPatch1: bad.patch\nPatch2: fuzz.patch\n\
                    Patch3: up.patch\n%prep\n%autosetup -p1\n";
    let before = std::fs::read(dir.path().join("fuzz.patch")).unwrap();
    let report = check(spec, dir.path(), Some(&tar), Refresh::Fuzz);
    assert_eq!(report.not_run, None, "{report:?}");
    let status = |n: &str| {
        report
            .patches
            .iter()
            .find(|p| p.name == n)
            .unwrap_or_else(|| panic!("{n} missing: {report:?}"))
    };
    assert_eq!(status("good.patch").status, PatchStatus::Applies);
    match &status("bad.patch").status {
        PatchStatus::Fails { output, rejects } => {
            assert!(output.contains("FAILED"), "{output}");
            assert!(rejects.contains("+TWO"), "{rejects}");
        }
        other => panic!("{other:?}"),
    }
    // Tried on a tree without the failing patch before it.
    let fuzz = status("fuzz.patch");
    assert!(
        matches!(
            fuzz.status,
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. }
        ),
        "{fuzz:?}"
    );
    assert!(
        matches!(
            status("up.patch").status,
            PatchStatus::AlreadyApplied { .. }
        ),
        "{report:?}"
    );
    assert!(report.proves_applied("up.patch") && !report.proves_applied("bad.patch"));
    let refreshed = String::from_utf8(fuzz.refreshed.clone().unwrap()).unwrap();
    assert!(refreshed.starts_with("Keep this header"), "{refreshed}");
    assert!(refreshed.contains("+G"), "{refreshed}");
    // The checkout is never written.
    assert_eq!(
        std::fs::read(dir.path().join("fuzz.patch")).unwrap(),
        before
    );
}

#[test]
fn real_quilt_leaves_an_offset_only_patch_byte_identical() {
    if !quilt_available() {
        eprintln!("quilt is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tar = tarball(dir.path(), &[("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\n")]);
    // The hunk says line 4, the text is at line 1: quilt applies it with an
    // offset and no fuzz.
    let offset = "--- a/f.txt\n+++ b/f.txt\n@@ -4,3 +4,3 @@\n a\n-b\n+B\n c\n";
    std::fs::write(dir.path().join("off.patch"), offset).unwrap();
    let spec = "Name: foo\nVersion: 1.0\nPatch0: off.patch\n%prep\n%autosetup -p1\n";
    let report = check(spec, dir.path(), Some(&tar), Refresh::Fuzz);
    assert_eq!(report.not_run, None, "{report:?}");
    let p = &report.patches[0];
    assert!(
        matches!(
            p.status,
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz: false, .. }
        ),
        "{p:?}"
    );
    assert_eq!(p.refreshed, None, "an offset alone is never refreshed");
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("off.patch")).unwrap(),
        offset
    );
}

#[test]
fn refresh_keeps_the_patch_shape_it_was_written_in() {
    let a = b"--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-a\n+b\n";
    assert_eq!(
        refresh_args(a, 1),
        ["-p", "ab", "--no-timestamps", "--no-index"]
    );
    let orig = b"Index: f.txt\n====\n--- foo-1.0.orig/f.txt\t2020-01-01\n+++ foo-1.0/f.txt\n";
    assert_eq!(refresh_args(orig, 1), ["--no-timestamps"]);
    let zero = b"--- f.txt\n+++ f.txt\n";
    assert_eq!(
        refresh_args(zero, 0),
        ["-p", "0", "--no-timestamps", "--no-index"]
    );
    // The description above the diff may hold a `---` separator of its own.
    let sep = b"Subject: x\n---\n--- a/f.txt\n+++ b/f.txt\n";
    assert_eq!(
        refresh_args(sep, 1),
        ["-p", "ab", "--no-timestamps", "--no-index"]
    );
}

#[test]
fn real_quilt_refresh_keeps_the_header_index_and_path_style() {
    if !quilt_available() {
        eprintln!("quilt is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tar = tarball(dir.path(), &[("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\n")]);
    // A fuzzy hunk (its first and last context lines do not match), in the
    // `<dir>.orig/` style with an Index line and a timestamp.
    let patch = "Keep this header\n\nIndex: f.txt\n\
        ===================================================================\n\
        --- foo-1.0.orig/f.txt\t2020-01-01 00:00:00.000000000 +0000\n\
        +++ foo-1.0/f.txt\t2020-01-01 00:00:00.000000000 +0000\n\
        @@ -6,5 +6,5 @@\n X\n f\n-g\n+G\n h\n Y\n";
    std::fs::write(dir.path().join("fuzzy.patch"), patch).unwrap();
    let spec = "Name: foo\nVersion: 1.0\nPatch0: fuzzy.patch\n%prep\n%autosetup -p1\n";
    let report = check(spec, dir.path(), Some(&tar), Refresh::Fuzz);
    assert_eq!(report.not_run, None, "{report:?}");
    let refreshed = String::from_utf8(report.patches[0].refreshed.clone().unwrap()).unwrap();
    assert!(refreshed.starts_with("Keep this header\n"), "{refreshed}");
    assert!(
        refreshed.lines().any(|l| l.starts_with("Index: ")),
        "the Index line is kept: {refreshed}"
    );
    assert!(
        refreshed.lines().any(|l| l.starts_with("=====")),
        "{refreshed}"
    );
    assert!(
        refreshed.lines().any(|l| l == "--- foo-1.0.orig/f.txt"),
        "the .orig style is kept, with no timestamp: {refreshed}"
    );
    assert!(
        refreshed
            .lines()
            .any(|l| l.starts_with("+++ foo-1.0/f.txt")),
        "{refreshed}"
    );
    assert!(
        !refreshed.contains("a/f.txt") && !refreshed.contains("2020-01-01"),
        "{refreshed}"
    );
}

#[test]
fn hunks_end_at_their_line_counts_so_a_removed_dash_line_is_not_a_header() {
    let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,1 @@\n--- x\n-y\n+z\n";
    assert_eq!(
        hunks(diff, 1),
        vec![Hunk {
            file: Some("f".into()),
            old: Some("f".into()),
            lines: vec![
                HunkLine::Removed("-- x".into()),
                HunkLine::Removed("y".into()),
                HunkLine::Added("z".into()),
            ],
        }]
    );
    // A created file has no name to prove against, a deleted one no new name.
    let created = "--- /dev/null\n+++ b/new.h\n@@ -0,0 +1 @@\n+int x;\n";
    assert_eq!(hunks(created, 1)[0].file, Some("new.h".into()));
    assert_eq!(hunks(created, 1)[0].old, None);
    let deleted = "--- a/old.h\n+++ /dev/null\n@@ -1 +0,0 @@\n-int x;\n";
    assert_eq!(hunks(deleted, 1)[0].file, None);
    assert_eq!(hunks(deleted, 1)[0].old, Some("old.h".into()));
}

#[test]
fn a_patch_whose_added_lines_are_in_the_new_archive_is_already_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let tar = tarball(dir.path(), &[("f.txt", "a\nint answer = 42;\nc\nd\n")]);
    let adds = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-int answer = 41;\n+int answer = 42;\n c\n d\n";
    assert!(already_in_tarball(&tar, adds.as_bytes(), 1));
    // The added line is not in the archive.
    let adds_other =
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-x\n+int answer = 99;\n c\n d\n";
    assert!(!already_in_tarball(&tar, adds_other.as_bytes(), 1));
    // A file the archive does not have is not proven either.
    let other = "--- a/g.txt\n+++ b/g.txt\n@@ -1 +1 @@\n-x\n+int answer = 42;\n";
    assert!(!already_in_tarball(&tar, other.as_bytes(), 1));
}

#[test]
fn trivial_added_lines_prove_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // Every file has braces and blank lines: they must not prove a hunk.
    let tar = tarball(dir.path(), &[("f.txt", "a\n}\n\n{\nbreak;\nc\n")]);
    for added in ["}", "{", "", "break;", "}\n+break;"] {
        let diff = format!("--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-x\n+{added}\n");
        assert!(!already_in_tarball(&tar, diff.as_bytes(), 1), "{added:?}");
    }
}

#[test]
fn added_lines_must_be_one_contiguous_run_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let tar = tarball(
        dir.path(),
        &[(
            "f.txt",
            "int first_value = 1;\nfiller line\nint second_value = 2;\nint third_value = 3;\n",
        )],
    );
    // Both lines are in the file, but not next to each other.
    let apart = "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1,2 @@\n-a\n+int first_value = 1;\n+int second_value = 2;\n";
    assert!(!already_in_tarball(&tar, apart.as_bytes(), 1));
    // Next to each other, but in the wrong order.
    let reversed = "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1,2 @@\n-a\n+int third_value = 3;\n+int second_value = 2;\n";
    assert!(!already_in_tarball(&tar, reversed.as_bytes(), 1));
    // Contiguous and in order.
    let run = "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1,2 @@\n-a\n+int second_value = 2;\n+int third_value = 3;\n";
    assert!(already_in_tarball(&tar, run.as_bytes(), 1));
}

#[test]
fn the_strip_level_decides_which_file_a_patch_is_proven_against() {
    let dir = tempfile::tempdir().unwrap();
    // `util.c` at the root has the line; `src/util.c` does not.
    let tar = tarball(
        dir.path(),
        &[
            ("util.c", "int answer = 42;\n"),
            ("src/util.c", "int other = 1;\n"),
        ],
    );
    let diff =
        |name: &str, line: &str| format!("--- {name}\n+++ {name}\n@@ -1 +1 @@\n-x\n+{line}\n");
    // -p0: `src/util.c` is the file, not the root `util.c`.
    let p0 = diff("src/util.c", "int answer = 42;");
    assert!(!already_in_tarball(&tar, p0.as_bytes(), 0));
    assert!(already_in_tarball(
        &tar,
        diff("util.c", "int answer = 42;").as_bytes(),
        0
    ));
    // -p1 strips `src/`: the root `util.c`.
    assert!(already_in_tarball(&tar, p0.as_bytes(), 1));
    // -p2 strips two components: `src/util.c`; one too few or too many is
    // another file.
    let p2 = diff("x/y/src/util.c", "int other = 1;");
    assert!(already_in_tarball(&tar, p2.as_bytes(), 2));
    assert!(!already_in_tarball(&tar, p2.as_bytes(), 1));
    assert!(!already_in_tarball(&tar, p2.as_bytes(), 3));
    // Fewer components than the level: no file.
    assert!(!already_in_tarball(&tar, p0.as_bytes(), 2));
    // A name that leaves the tree proves nothing.
    let up = diff("a/../../util.c", "int answer = 42;");
    assert!(!already_in_tarball(&tar, up.as_bytes(), 1));
}
