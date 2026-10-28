use super::*;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn a_forge_commit_url_becomes_the_patch_the_forge_serves() {
    assert_eq!(
        patch_url(&format!("https://github.com/curl/curl/commit/{SHA}")).unwrap(),
        format!("https://github.com/curl/curl/commit/{SHA}.patch")
    );
    assert_eq!(
        patch_url("https://github.com/curl/curl/pull/1234").unwrap(),
        "https://github.com/curl/curl/pull/1234.patch"
    );
    assert_eq!(
        patch_url(&format!("https://github.com/o/r/pull/9/commits/{SHA}#diff")).unwrap(),
        format!("https://github.com/o/r/commit/{SHA}.patch")
    );
    assert_eq!(
        patch_url(&format!(
            "https://gitlab.gnome.org/GNOME/libxml2/-/commit/{SHA}"
        ))
        .unwrap(),
        format!("https://gitlab.gnome.org/GNOME/libxml2/-/commit/{SHA}.patch")
    );
    assert_eq!(
        patch_url("https://gitlab.com/a/b/c/-/merge_requests/7").unwrap(),
        "https://gitlab.com/a/b/c/-/merge_requests/7.patch"
    );
    assert_eq!(
        patch_url("https://codeberg.org/o/r/commit/abc1234").unwrap(),
        "https://codeberg.org/o/r/commit/abc1234.patch"
    );
    // A patch URL is kept, query and all, without its fragment.
    assert_eq!(
        patch_url("https://example.org/fix.diff?raw=1#top").unwrap(),
        "https://example.org/fix.diff?raw=1"
    );
    for bad in [
        "http://github.com/o/r/commit/abc1234.patch",
        "https://github.com/o/r",
        "https://github.com/o/r/commit/not-a-sha",
        "https://example.org/page.html",
        "ftp://x/y.patch",
    ] {
        assert!(patch_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_commit_id_needs_a_known_forge_repository() {
    assert_eq!(
        commit_patch_url("https://github.com/curl/curl.git", "abc1234").unwrap(),
        "https://github.com/curl/curl/commit/abc1234.patch"
    );
    assert_eq!(
        commit_patch_url("https://gitlab.gnome.org/GNOME/glib/", "abc1234").unwrap(),
        "https://gitlab.gnome.org/GNOME/glib/-/commit/abc1234.patch"
    );
    assert!(commit_patch_url("https://git.kernel.org/pub/scm/x.git", "abc1234").is_err());
    assert!(commit_patch_url("http://github.com/o/r", "abc1234").is_err());
    assert!(commit_patch_url("https://github.com/o/r", "xyz").is_err());
    assert!(commit_patch_url("https://github.com/o", "abc1234").is_err());
}

#[test]
fn backport_arguments_are_a_file_an_https_url_or_a_commit_with_its_repo() {
    assert_eq!(
        parse_fix_source("fix.patch", None).unwrap(),
        FixSource::File("fix.patch".into())
    );
    assert_eq!(
        parse_fix_source(&format!("https://github.com/o/r/commit/{SHA}"), None).unwrap(),
        FixSource::Url(format!("https://github.com/o/r/commit/{SHA}.patch"))
    );
    assert_eq!(
        parse_fix_source("ABC1234", Some("https://github.com/o/r")).unwrap(),
        FixSource::Commit {
            repo: "https://github.com/o/r".into(),
            id: "abc1234".into()
        }
    );
    assert!(
        parse_fix_source("abc1234", None)
            .unwrap_err()
            .contains("--backport-repo")
    );
    assert!(parse_fix_source("http://x/y.patch", None).is_err());
    assert!(parse_fix_source("file:///etc/passwd", None).is_err());
    assert!(parse_fix_source("  ", None).is_err());
}

#[test]
fn patch_names_come_from_the_cve_or_the_commit() {
    let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
    assert_eq!(
        patch_file_name(&refs, Some(SHA), 0, 1),
        "CVE-2025-1234.patch"
    );
    assert_eq!(
        patch_file_name(&refs, Some(SHA), 1, 2),
        "CVE-2025-1234-0123456.patch"
    );
    assert_eq!(patch_file_name(&refs, None, 1, 2), "CVE-2025-1234-2.patch");
    let bug = vec!["bsc#1234567".to_string()];
    assert_eq!(patch_file_name(&bug, Some(SHA), 0, 1), "fix-0123456.patch");
    assert_eq!(patch_file_name(&bug, Some(SHA), 0, 2), "fix-0123456.patch");
    assert_eq!(patch_file_name(&bug, None, 0, 1), "bsc1234567.patch");
    assert_eq!(
        patch_file_name(&["jsc#SLE-12".to_string()], None, 0, 1),
        "jscSLE-12.patch"
    );
}

const PATCH: &str = "From 0123456789abcdef0123456789abcdef01234567 Mon Sep 17 00:00:00 2001
From: Dev <dev@example.org>
Subject: [PATCH 1/2] http: reject a header line that is too
 long
---
 lib/http.c | 1 +
--- a/lib/http.c
+++ b/lib/http.c
@@ -1,1 +1,2 @@
 a
+b
";

#[test]
fn a_format_patch_names_its_commit_and_subject() {
    assert_eq!(commit_in_patch(PATCH).as_deref(), Some(SHA));
    assert_eq!(
        subject_of_patch(PATCH).as_deref(),
        Some("http: reject a header line that is too long")
    );
    assert_eq!(subject_of_patch("--- a\n+++ b\n@@ -1 +1 @@\n"), None);
    assert_eq!(
        commit_id_in_url(&format!("https://github.com/o/r/commit/{SHA}.patch")).as_deref(),
        Some(SHA)
    );
    assert_eq!(commit_id_in_url("https://example.org/fix.patch"), None);
    assert_eq!(
        reason_for(None, Some(SHA)),
        "backport upstream commit 0123456789ab"
    );
    assert_eq!(reason_for(Some("Fix it."), None), "Fix it");
}

#[test]
fn only_a_unified_diff_is_a_patch() {
    assert!(check_patch(PATCH.as_bytes()).is_ok());
    assert!(check_patch(b"<!DOCTYPE html><html>sign in</html>").is_err());
    assert!(check_patch(b"just text").is_err());
    let big = vec![b'a'; MAX_PATCH_BYTES as usize + 1];
    assert!(check_patch(&big).unwrap_err().contains("larger"));
}

#[test]
fn a_staged_file_is_written_kept_or_refused() {
    assert_eq!(stage_decision(None, b"a", false), Stage::Write);
    assert_eq!(stage_decision(Some(b"a"), b"a", true), Stage::Same);
    assert_eq!(
        stage_decision(Some(b"b"), b"a", false),
        Stage::KeepRefreshed
    );
    assert_eq!(stage_decision(Some(b"b"), b"a", true), Stage::Conflict);
    assert_eq!(stage_decision(None, b"a", true), Stage::Conflict);
}

fn prepared() -> PreparedPatch {
    PreparedPatch {
        file_name: "CVE-2025-1234.patch".into(),
        bytes: PATCH.as_bytes().to_vec(),
        origin: "https://github.com/o/r/commit/x.patch".into(),
        commit: Some(SHA.into()),
        reason: "reject overlong header lines".into(),
    }
}

#[test]
fn the_spec_comment_the_bullet_and_the_task_name_patch_and_references() {
    let refs = vec!["bsc#123456".to_string(), "CVE-2025-1234".to_string()];
    let p = prepared();
    assert_eq!(
        spec_comment(&p, &refs),
        "# PATCH-FIX-UPSTREAM CVE-2025-1234.patch bsc#123456 CVE-2025-1234 -- reject overlong header lines"
    );
    assert_eq!(
        changes_bullet(&p, &refs),
        "- Add CVE-2025-1234.patch: reject overlong header lines (bsc#123456, CVE-2025-1234)"
    );
    let job = Job {
        patches: vec![p],
        refs,
    };
    let task = job.problem("curl", "8.6.0");
    assert!(task.contains("stays at version 8.6.0"), "{task}");
    assert!(task.contains("Do not change `Version:`"), "{task}");
    assert!(
        task.contains("# PATCH-FIX-UPSTREAM CVE-2025-1234.patch"),
        "{task}"
    );
    assert!(task.contains("%autosetup"), "{task}");
    let dry = dry_run_lines(&job, "curl", "8.6.0", "home:u:branches:X:Update");
    assert!(dry[0].contains("the version stays 8.6.0"), "{dry:?}");
}

const CHANGES: &str = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Add CVE-2025-1234.patch to fix the header parser
  that crashed
- Add BuildRequires: zlib

-------------------------------------------------------------------
Mon Sep  1 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 8.6.0
";

#[test]
fn the_pipeline_writes_the_backport_bullet_in_the_newest_entry() {
    let b = (
        "CVE-2025-1234.patch".to_string(),
        "- Add CVE-2025-1234.patch: reject overlong header lines (bsc#123456, CVE-2025-1234)"
            .to_string(),
    );
    let got = with_backport_bullets(CHANGES, std::slice::from_ref(&b)).unwrap();
    assert!(
        got.contains(&format!(
            "<dev@suse.com>\n\n{}\n- Add BuildRequires: zlib\n",
            b.1
        )),
        "{got}"
    );
    assert!(!got.contains("that crashed"), "{got}");
    assert!(got.ends_with("- Update to 8.6.0\n"));
    assert_eq!(
        with_backport_bullets(&got, &[b.clone()]),
        None,
        "idempotent"
    );
    // An entry that does not name the file gets the bullet first.
    let other = CHANGES.replace("CVE-2025-1234.patch to fix", "the fix of");
    let got = with_backport_bullets(&other, &[b.clone()]).unwrap();
    assert!(
        got.contains(&format!("<dev@suse.com>\n\n{}\n- Add the fix of", b.1)),
        "{got}"
    );
    assert_eq!(with_backport_bullets("no entry", &[b]), None);
}

#[test]
fn a_backport_is_clean_refreshed_or_missing() {
    let p = prepared();
    let spec = "Name: curl\nVersion: 8.6.0\nSource: curl.tar.xz\n\
                    # PATCH-FIX-UPSTREAM CVE-2025-1234.patch bsc#1 -- x\n\
                    Patch1: CVE-2025-1234.patch\n%prep\n%autosetup -p1\n";
    assert_eq!(classify(&p, Some(PATCH), spec), Applied::Clean);
    let refreshed = PATCH.replace("@@ -1,1 +1,2 @@", "@@ -3,1 +3,2 @@");
    assert_eq!(classify(&p, Some(&refreshed), spec), Applied::Refreshed);
    assert!(matches!(classify(&p, None, spec), Applied::Missing(_)));
    let unapplied = "Name: curl\nVersion: 8.6.0\n%prep\n%setup -q\n";
    assert!(matches!(
        classify(&p, Some(PATCH), unapplied),
        Applied::Missing(w) if w.contains("does not apply")
    ));
}

#[test]
fn staging_writes_the_patch_into_the_checkout_and_refuses_a_taken_name() {
    // Under the workspace's target directory, not /tmp.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target")
        .join(format!("backport-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let job = Job {
        patches: vec![prepared()],
        refs: vec!["CVE-2025-1234".into()],
    };
    let notes = backport_patches(&dir, &job, &[]).unwrap();
    assert!(
        notes[0].starts_with("added CVE-2025-1234.patch"),
        "{notes:?}"
    );
    assert_eq!(
        std::fs::read(dir.join("CVE-2025-1234.patch")).unwrap(),
        PATCH.as_bytes()
    );
    let again = backport_patches(&dir, &job, &[]).unwrap();
    assert!(again[0].contains("already"), "{again:?}");
    std::fs::write(dir.join("CVE-2025-1234.patch"), "other").unwrap();
    assert!(backport_patches(&dir, &job, &["CVE-2025-1234.patch".into()]).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
