//! The removal gate without quilt: the security names, and the removals quilt
//! did not check, which are refused.

use super::removal_proof::*;
use crate::patch_check::{CheckReport, DropProof, PatchResult, PatchStatus};

#[test]
fn a_security_patch_is_named_by_its_name_or_its_header() {
    assert!(is_security_patch("fix-CVE-2024-1234.patch", b""));
    assert!(is_security_patch("bsc#1234.patch", b""));
    assert!(is_security_patch("boo1234-x.patch", b""));
    assert!(is_security_patch(
        "fix-foo.patch",
        b"Subject: x\n\nFixes CVE-2024-5678.\n"
    ));
    assert!(is_security_patch("fix-foo.patch", b"Upstream: bsc#99\n"));
    // `boost-*` and an id after the first 40 lines are not security fixes.
    assert!(!is_security_patch("boost-thread.patch", b""));
    assert!(!is_security_patch("fix-foo.patch", b""));
    let late = format!("{}CVE-2024-5678-1\n", "x\n".repeat(40));
    assert!(!is_security_patch("fix-foo.patch", late.as_bytes()));
}

#[test]
fn a_glued_cve_name_is_named_and_a_boo_before_a_digit_word_is_not() {
    assert!(is_security_patch("cve2024-1234-fix.patch", b""));
    assert!(is_security_patch("fix-cve2024-1234.patch", b""));
    // `boo` followed by a token that is not a number is a word, not a bug id.
    assert!(!is_security_patch("foo-boo-2d.patch", b""));
    assert!(!is_security_patch("boo-2d-fix.patch", b""));
}

/// A checkout with `p.patch` in its spec, and a spec that removes it (the file
/// and its line gone). Returns the checkout, the spec path and the spec before.
fn removal(spec_uses: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path().to_path_buf();
    let before = format!("Name: foo\nVersion: 1.0\nPatch0: p.patch\n%prep\n{spec_uses}\n");
    std::fs::write(checkout.join("p.patch"), "--- a/f\n+++ b/f\n").unwrap();
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, &before).unwrap();
    std::fs::remove_file(checkout.join("p.patch")).unwrap();
    std::fs::write(
        &spec_path,
        format!("Name: foo\nVersion: 1.0\n%prep\n{spec_uses}\n"),
    )
    .unwrap();
    (dir, spec_path, before)
}

#[test]
fn a_removal_quilt_did_not_run_on_is_refused() {
    // No source archive in the checkout: quilt cannot check the patch.
    let (dir, spec_path, before) = removal("%autosetup -p1");
    let judged = judge_removals("1.0", dir.path(), &spec_path, &before, None);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
    assert_eq!(judged.refused[0].name, "p.patch");
    assert!(
        judged.refused[0].reason.contains("did not check"),
        "{judged:?}"
    );
}

#[test]
fn a_patch_at_a_level_quilt_does_not_check_is_refused() {
    let (dir, spec_path, before) = removal("%autosetup -p2");
    let judged = judge_removals("1.0", dir.path(), &spec_path, &before, None);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert!(judged.refused[0].reason.contains("-p2"), "{judged:?}");
}

#[test]
fn an_unchecked_security_patch_is_refused_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path().to_path_buf();
    let before = "Name: foo\nVersion: 1.0\nPatch0: CVE-2024-1.patch\n%prep\n%autosetup -p1\n";
    std::fs::write(checkout.join("CVE-2024-1.patch"), "--- a/f\n+++ b/f\n").unwrap();
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, before).unwrap();
    std::fs::remove_file(checkout.join("CVE-2024-1.patch")).unwrap();
    std::fs::write(
        &spec_path,
        "Name: foo\nVersion: 1.0\n%prep\n%autosetup -p1\n",
    )
    .unwrap();
    let judged = judge_removals("1.0", &checkout, &spec_path, before, None);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
    assert!(judged.refused[0].security, "{judged:?}");
}

#[test]
fn the_refusal_text_states_why_no_proof_was_made() {
    let text = unproven_removal_problem(&[Refusal {
        name: "p.patch".into(),
        kind: RemovalKind::Removed,
        security: false,
        renamed_to: None,
        reason: "quilt checked it, it cannot be reverse-applied".into(),
    }]);
    assert!(text.contains("quilt checked it"), "{text}");
    assert!(!text.contains("fails and is not upstream"), "{text}");
    let unchecked = unproven_removal_problem(&[Refusal {
        name: "p.patch".into(),
        kind: RemovalKind::Removed,
        security: true,
        renamed_to: None,
        reason: "quilt did not check it (no source archive)".into(),
    }]);
    assert!(unchecked.contains("did not check"), "{unchecked}");
    assert!(unchecked.contains("security patch"), "{unchecked}");
}

/// A `foo-<version>.tar.gz` in `dir` with the files `(name, body)` under `foo-<version>/`.
fn archive(dir: &std::path::Path, name: &str, files: &[(&str, &str)]) {
    let file = std::fs::File::create(dir.join(name)).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for (path, body) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, format!("foo-1.0/{path}"), body.as_bytes())
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
}

#[test]
fn a_patch_in_a_subdirectory_is_judged_and_refused() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    std::fs::create_dir(checkout.join("sub")).unwrap();
    std::fs::write(checkout.join("sub/x.patch"), "--- a/f\n+++ b/f\n").unwrap();
    let before = "Name: foo\nVersion: 1.0\nPatch0: sub/x.patch\n%prep\n%autosetup -p1\n";
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, before).unwrap();
    std::fs::remove_file(checkout.join("sub/x.patch")).unwrap();
    std::fs::write(
        &spec_path,
        "Name: foo\nVersion: 1.0\n%prep\n%autosetup -p1\n",
    )
    .unwrap();
    let judged = judge_removals("1.0", checkout, &spec_path, before, None);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
    assert_eq!(judged.refused[0].name, "sub/x.patch");
    // A name that leaves the checkout is not a patch file.
    assert!(is_checkout_file("sub/x.patch"));
    assert!(!is_checkout_file("../x.patch") && !is_checkout_file("/x.patch"));
    assert!(!is_checkout_file(""));
}

#[test]
fn a_proof_is_made_only_against_an_archive_of_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    // Only the old version's archive and an unnamed one: neither is the new one.
    archive(checkout, "foo-0.9.tar.gz", &[]);
    archive(checkout, "master.tar.gz", &[]);
    assert_eq!(proof_tarball(checkout, "1.0+git2"), None);
    archive(checkout, "foo-1.0+git2.tar.gz", &[]);
    let named = proof_tarball(checkout, "1.0+git2").unwrap();
    assert!(named.ends_with("foo-1.0+git2.tar.gz"));
    // The only archive there is can only be the new one.
    let alone = tempfile::tempdir().unwrap();
    archive(alone.path(), "master.tar.gz", &[]);
    assert!(proof_tarball(alone.path(), "1.0+git2").is_some());
}

#[test]
fn a_p0_patch_is_not_proven_by_a_file_at_the_archive_root() {
    let quilt = std::process::Command::new("quilt")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !quilt {
        eprintln!("quilt is not installed; skipping the -p0 removal proof");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    // `util.c` at the root has the line, `src/util.c` is not in the archive.
    archive(
        checkout,
        "foo-1.0.tar.gz",
        &[("util.c", "int answer_value = 42;\n")],
    );
    let patch = "--- src/util.c\n+++ src/util.c\n@@ -1 +1 @@\n-int answer_value = 41;\n+int answer_value = 42;\n";
    std::fs::write(checkout.join("p.patch"), patch).unwrap();
    let before = "Name: foo\nVersion: 1.0\nPatch0: p.patch\n%prep\n%setup -q\n%patch -P 0 -p0\n";
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, before).unwrap();
    std::fs::remove_file(checkout.join("p.patch")).unwrap();
    std::fs::write(&spec_path, "Name: foo\nVersion: 1.0\n%prep\n%setup -q\n").unwrap();
    let judged = judge_removals("1.0", checkout, &spec_path, before, None);
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
}

#[test]
fn a_rename_is_refused_with_its_own_text() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    let before = "Name: foo\nVersion: 1.0\nPatch0: old.patch\n%prep\n%autosetup -p1\n";
    std::fs::write(checkout.join("old.patch"), "--- a/f\n+++ b/f\n").unwrap();
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, before).unwrap();
    let snapshot = crate::pipeline::snapshot::CheckoutSnapshot::take(checkout);
    // The same bytes under another name, declared in place of the old one.
    std::fs::rename(checkout.join("old.patch"), checkout.join("new.patch")).unwrap();
    std::fs::write(
        &spec_path,
        "Name: foo\nVersion: 1.0\nPatch0: new.patch\n%prep\n%autosetup -p1\n",
    )
    .unwrap();
    let judged = judge_removals("1.0", checkout, &spec_path, before, Some(&snapshot));
    assert!(judged.proven.is_empty(), "{judged:?}");
    assert_eq!(judged.refused.len(), 1, "{judged:?}");
    assert_eq!(judged.refused[0].renamed_to.as_deref(), Some("new.patch"));
    let text = unproven_removal_problem(&judged.refused);
    assert!(text.contains("looks renamed"), "{text}");
    assert!(!text.contains("refresh it with quilt"), "{text}");
}

#[test]
fn a_wheel_switch_drops_its_patches_without_a_proof_and_an_edit_does_not() {
    let (dir, spec_path, before) = removal("%autosetup -p1");
    let step = ap_core::run::StepDir {
        path: dir.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let after = std::fs::read_to_string(&spec_path).unwrap();
    let judge = |judge_removals: bool| {
        crate::pipeline::patch_rules::reject_patch_rules(
            &spec_path,
            &before,
            None,
            &step,
            &ap_core::sink::NullReporter,
            dir.path().join("notes.md"),
            &[],
            judge_removals,
        )
    };
    // An edit that removes the patch is refused (no archive to prove it by).
    assert!(judge(true).is_some_and(|w| w.contains("removal of p.patch is refused")));
    // The wheel switch's own spec rewrite is not judged, and nothing is rolled back.
    std::fs::write(&spec_path, &after).unwrap();
    assert_eq!(judge(false), None);
    assert_eq!(std::fs::read_to_string(&spec_path).unwrap(), after);
}

/// A report on one patch `name` that quilt failed, with `output` as its text.
fn failed(name: &str, output: &str) -> CheckReport {
    CheckReport {
        patches: vec![PatchResult {
            name: name.into(),
            level: 1,
            status: PatchStatus::Fails {
                output: output.into(),
                rejects: String::new(),
            },
            refreshed: None,
        }],
        not_run: None,
    }
}

#[test]
fn a_patch_quilt_reverse_applies_only_with_fuzz_is_proven_by_its_hunks() {
    let dir = tempfile::tempdir().unwrap();
    // The first hunk's context differs from the archive (quilt's fuzz) but its
    // changed lines are there; the second deletes a file the archive lacks.
    archive(
        dir.path(),
        "foo-1.0.tar.gz",
        &[("f.py", "import os\nwarn('new')\nint answer = 42;\nend\n")],
    );
    let tar = dir.path().join("foo-1.0.tar.gz");
    let diff = b"--- a/f.py\n+++ b/f.py\n@@ -1,3 +1,3 @@\n import sys\n-int answer = 41;\n+int answer = 42;\n end\n\
                 --- a/gone.py\n+++ /dev/null\n@@ -1 +0,0 @@\n-import removed_module\n";
    let fuzzy = failed(
        "p.patch",
        "Patching file f.py with fuzz 2\nPatch p.patch can be reverse-applied\n",
    );
    assert_eq!(
        proof_of(&fuzzy, Some(&tar), Some(diff), 1, "p.patch"),
        Ok(DropProof::InTarball)
    );
    // The added line differs in the archive: quilt's fuzz match is no proof.
    let differs = b"--- a/f.py\n+++ b/f.py\n@@ -1,3 +1,3 @@\n import sys\n-int answer = 41;\n+int answer = 43;\n end\n";
    let refused = proof_of(&fuzzy, Some(&tar), Some(differs), 1, "p.patch").unwrap_err();
    assert!(refused.contains("only with fuzz"), "{refused}");
    // Quilt's reverse-apply alone, with nothing to check the hunks against, is no proof.
    assert!(proof_of(&fuzzy, None, Some(diff), 1, "p.patch").is_err());
}

#[test]
fn a_web_address_patch_names_the_file_it_ends_with() {
    let spec = "Name: foo\nVersion: 1.0\nPatch0: https://example.org/x/%{name}-%{version}.patch\n";
    assert_eq!(
        crate::pipeline::expand_patch_name("https://example.org/x/%{name}-%{version}.patch", spec),
        "foo-1.0.patch"
    );
    // A plain name is what it was.
    assert_eq!(
        crate::pipeline::expand_patch_name("foo-%{version}-fix.patch", spec),
        "foo-1.0-fix.patch"
    );
}

#[test]
fn a_web_address_patch_is_removed_with_its_quilt_proof() {
    if std::process::Command::new("quilt")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    archive(
        checkout,
        "foo-1.0.tar.gz",
        &[("f.txt", "a\nint answer = 42;\nc\nd\n")],
    );
    // The change is already in the archive: quilt reverse-applies the file.
    let patch = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-int answer = 41;\n+int answer = 42;\n c\n d\n";
    std::fs::write(checkout.join("foo-1.0.patch"), patch).unwrap();
    let before = "Name: foo\nVersion: 1.0\nPatch0: https://example.org/x/%{name}-%{version}.patch\n%prep\n%autosetup -p1\n";
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, before).unwrap();
    std::fs::write(
        &spec_path,
        "Name: foo\nVersion: 1.0\n%prep\n%autosetup -p1\n",
    )
    .unwrap();
    let judged = judge_removals("1.0", checkout, &spec_path, before, None);
    assert!(judged.refused.is_empty(), "{judged:?}");
    assert_eq!(
        judged.proven,
        vec![("foo-1.0.patch".to_string(), DropProof::ReverseApplies)]
    );
}

#[test]
fn a_patch_whose_every_file_is_gone_from_the_archive_is_proven() {
    let dir = tempfile::tempdir().unwrap();
    archive(
        dir.path(),
        "foo-1.0.tar.gz",
        &[("other.py", "int unrelated_line = 5;\n")],
    );
    let tar = dir.path().join("foo-1.0.tar.gz");
    let gone =
        b"--- a/gone.py\n+++ b/gone.py\n@@ -1 +1 @@\n-import old_module\n+import new_module\n";
    let fails = failed("p.patch", "Patching file gone.py\nfailed\n");
    assert_eq!(
        proof_of(&fails, Some(&tar), Some(gone), 1, "p.patch"),
        Ok(DropProof::TargetsAbsent)
    );
    // One of its two files is still in the archive: refused.
    let mixed = b"--- a/other.py\n+++ b/other.py\n@@ -1 +1 @@\n-x\n+y\n--- a/gone.py\n+++ b/gone.py\n@@ -1 +1 @@\n-x\n+y\n";
    assert!(proof_of(&fails, Some(&tar), Some(mixed), 1, "p.patch").is_err());
    // A security patch is not dropped for its files being gone.
    let cve = failed("fix-CVE-2024-1234.patch", "Patching file gone.py\nfailed\n");
    assert!(proof_of(&cve, Some(&tar), Some(gone), 1, "fix-CVE-2024-1234.patch").is_err());
    // Without the archive nothing is judged.
    assert!(proof_of(&fails, None, Some(gone), 1, "p.patch").is_err());
}

#[test]
fn a_patch_renamed_by_a_fragment_is_judged_by_the_renamed_file() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path();
    let spec = "Name: easydev\nVersion: 0.12\nPatch:  https://github.com/cokelaer/easydev/commit/8cf50c9177936e771e158ea9b249b4aa5e2cc1ee.patch#/fix-version.patch\n%prep\n%autosetup -p1\n";
    std::fs::write(checkout.join("fix-version.patch"), "--- a/x\n+++ b/x\n").unwrap();
    let files = super::removal_proof::patch_files_of(checkout, spec, None);
    assert_eq!(files.keys().collect::<Vec<_>>(), vec!["fix-version.patch"]);
    assert!(files["fix-version.patch"].is_some());
    assert_eq!(
        crate::patch_check::declared_patches(spec)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>(),
        vec!["fix-version.patch"]
    );
    assert_eq!(
        crate::pipeline::declared_patches(spec)
            .into_iter()
            .map(|(_, _, n)| n)
            .collect::<Vec<_>>(),
        vec!["fix-version.patch"]
    );
}
