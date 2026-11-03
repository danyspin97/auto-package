//! The removal gate without quilt: the security names, and the removals quilt
//! did not check, which are refused.

use super::removal_proof::*;

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
        security: false,
        renamed_to: None,
        reason: "quilt checked it, it does not `can be reverse-applied`".into(),
    }]);
    assert!(text.contains("quilt checked it"), "{text}");
    assert!(!text.contains("fails and is not upstream"), "{text}");
    let unchecked = unproven_removal_problem(&[Refusal {
        name: "p.patch".into(),
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
