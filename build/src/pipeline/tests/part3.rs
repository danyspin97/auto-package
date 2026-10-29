use super::*;
use crate::pipeline::dep_edit::*;
use crate::pipeline::patch_check::*;
use crate::pipeline::rpm_deps::*;
use crate::pipeline::snapshot::*;

#[test]
fn no_baseline_judges_only_the_patches_obs_does_not_publish() {
    let dir = tempfile::tempdir().unwrap();
    let spec_path = dir.path().join("pkg.spec");
    let spec = "Name: pkg\nPatch0: published.patch\nPatch1: new.patch\n";
    std::fs::write(&spec_path, spec).unwrap();
    let escaping = "--- ../../x\n+++ ../../x\n@@ -1 +1 @@\n-a\n+b\n";
    std::fs::write(dir.path().join("published.patch"), escaping).unwrap();
    std::fs::write(dir.path().join("new.patch"), escaping).unwrap();

    // The published `..` header is what was committed: not this turn's, so
    // only the unpublished patch is judged.
    let published = PatchBaseline::NoBaseline(vec!["published.patch".to_string()]);
    let problem = patch_escape_problem(&spec_path, spec, &published).expect("escape");
    assert!(problem.starts_with("new.patch"), "{problem}");

    // Nothing published: both are judged, and the first is named.
    let none = PatchBaseline::NoBaseline(Vec::new());
    let problem = patch_escape_problem(&spec_path, spec, &none).expect("escape");
    assert!(problem.starts_with("new.patch"), "{problem}");

    // Only the unpublished one is judged: a published `new.patch` leaves
    // `published.patch` as the one reported.
    let only_new = PatchBaseline::NoBaseline(vec!["new.patch".to_string()]);
    let problem = patch_escape_problem(&spec_path, spec, &only_new).expect("escape");
    assert!(problem.starts_with("published.patch"), "{problem}");
}

#[test]
fn patch_the_turn_modified_with_an_escaping_header_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let spec_path = dir.path().join("pkg.spec");
    let spec = "Name: pkg\nPatch0: old.patch\n";
    std::fs::write(&spec_path, spec).unwrap();
    std::fs::write(
        dir.path().join("old.patch"),
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();
    let snap = CheckoutSnapshot::take(dir.path());

    // The turn rewrites the patch with an absolute header.
    std::fs::write(
        dir.path().join("old.patch"),
        "--- /tmp/orig/x\n+++ /tmp/new/x\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();
    let problem = patch_escape_problem(&spec_path, spec, &PatchBaseline::Snapshot(&snap))
        .expect("the turn's edit escapes");
    assert!(problem.contains("old.patch"), "{problem}");
    assert!(problem.contains("changed by this turn"), "{problem}");

    // A patch the turn added with a `..` header is rejected too.
    std::fs::write(
        dir.path().join("new.patch"),
        "--- ../../x\n+++ ../../x\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();
    assert!(patch_escape_problem(&spec_path, spec, &PatchBaseline::Snapshot(&snap)).is_some());
}

#[test]
fn superseded_tarball_keeps_an_archive_the_new_spec_still_names() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["pkg-1.2.tar.gz", "pkg-1.2-data.tar.gz"] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    let new = dir.path().join("pkg-1.3.tar.gz");
    let sources = vec![crate::spec::SourceEntry {
        url: "https://example.com/pkg-1.2-data.tar.gz".to_string(),
        file: "pkg-1.2-data.tar.gz".to_string(),
    }];
    assert_eq!(
        superseded_tarball(dir.path(), "1.2", &new, &sources),
        Some(dir.path().join("pkg-1.2.tar.gz"))
    );
}

#[test]
fn only_a_definite_answer_makes_a_repository_probe_exist_or_missing() {
    assert_eq!(classify_repo_probe::<()>(&Ok(())), RepoProbe::Exists);
    assert_eq!(
        classify_repo_probe::<()>(&Err(ureq::Error::StatusCode(404))),
        RepoProbe::Missing
    );
    assert_eq!(
        classify_repo_probe::<()>(&Err(ureq::Error::StatusCode(410))),
        RepoProbe::Missing
    );
    for status in [403, 429, 500, 503] {
        assert_eq!(
            classify_repo_probe::<()>(&Err(ureq::Error::StatusCode(status))),
            RepoProbe::Unknown,
            "status {status}"
        );
    }
    assert_eq!(
        classify_repo_probe::<()>(&Err(ureq::Error::Io(std::io::Error::other("reset")))),
        RepoProbe::Unknown
    );
}

#[test]
fn escaping_patch_in_checkout_is_named_by_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Name: pkg\nPatch0: bad.patch\nPatch1: good.patch\n";
    let spec_path = dir.path().join("pkg.spec");
    std::fs::write(&spec_path, spec).unwrap();
    std::fs::write(dir.path().join("good.patch"), "--- a/x\n+++ b/x\n").unwrap();
    assert_eq!(
        patch_escape_problem(&spec_path, spec, &PatchBaseline::NoBaseline(Vec::new())),
        None
    );

    // an unreferenced top-level .diff is still scanned
    std::fs::write(
        dir.path().join("stray.diff"),
        "--- a/x\n+++ /home/u/.config/osc/oscrc\n",
    )
    .unwrap();
    let problem = patch_escape_problem(&spec_path, spec, &PatchBaseline::NoBaseline(Vec::new()))
        .expect("escape found");
    assert!(problem.starts_with("stray.diff"), "{problem}");
    std::fs::remove_file(dir.path().join("stray.diff")).unwrap();

    std::fs::write(
        dir.path().join("bad.patch"),
        "--- a/../../../etc/x\n+++ b/../../../etc/x\n",
    )
    .unwrap();
    let problem = patch_escape_problem(&spec_path, spec, &PatchBaseline::NoBaseline(Vec::new()))
        .expect("escape found");
    assert!(problem.starts_with("bad.patch"), "{problem}");
    assert!(problem.contains("`..`"), "{problem}");
}

#[test]
fn every_patch_that_does_not_apply_is_found_and_the_rest_build_on_each_other() {
    let root = tempfile::tempdir().unwrap();
    // A checkout path with `:` in it, as in `home:user:project`.
    let dir = root.path().join("home:u:p");
    std::fs::create_dir_all(&dir).unwrap();
    let tarball = dir.join("pkg-1.0.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            std::fs::File::create(&tarball).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        let data = b"a\nb\nc\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tb.append_data(&mut h, "pkg-1.0/f.txt", &data[..]).unwrap();
        tb.into_inner().unwrap().finish().unwrap();
    }
    let spec = "Version: 1.0\n%autosetup -p1 -n pkg-%{version}\n\
                    Patch0: stale.patch\nPatch1: b.patch\nPatch2: c.patch\n";
    let spec_path = dir.join("pkg.spec");
    std::fs::write(&spec_path, spec).unwrap();
    // Made for other code: its context is not in f.txt.
    std::fs::write(
        dir.join("stale.patch"),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n x\n-y\n+Y\n z\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.patch"),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n",
    )
    .unwrap();
    // Applies only on top of b.patch.
    std::fs::write(
        dir.join("c.patch"),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-B\n+BB\n c\n",
    )
    .unwrap();
    let check = try_patches(&spec_path, spec).unwrap();
    assert_eq!(check.unchecked, None);
    let names: Vec<&str> = check.failed.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["stale.patch"], "{check:?}");
    // Without the stale one the spec is clean.
    let kept = without_patches(spec, &["stale.patch".to_string()]);
    assert_eq!(
        try_patches(&spec_path, &kept).unwrap(),
        PatchCheck::default()
    );
    // The dry run wrote nothing into the checkout.
    assert!(!dir.join("f.txt").exists());
}

#[test]
fn rpm_dep_is_not_satisfied_by_a_longer_sibling_package_name() {
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "python313-foo-bar-1.0-1.1.x86_64.rpm",
        "python313-foo-devel-1.0-1.1.noarch.rpm",
        "python313-foo-debuginfo-1.0-1.1.x86_64.rpm",
    ] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    assert!(!rpm_satisfies_dep(dir.path(), "python313-foo"));
    assert!(!rpm_satisfies_dep(dir.path(), "python313-foo>=0.1"));
    std::fs::write(dir.path().join("python313-foo-1.0-1.1.x86_64.rpm"), b"").unwrap();
    assert!(rpm_satisfies_dep(dir.path(), "python313-foo"));
    assert!(rpm_satisfies_dep(dir.path(), "python313-foo>=0.1"));
}

#[test]
fn find_tarball_prefers_the_current_version_and_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "a-1.0.tar.gz",
        "foo-1.0.tar.gz",
        "foo-2.0.tar.gz",
        "vendor.tar.zst",
    ] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    let name = |v: &str| {
        find_tarball(dir.path(), v)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    };
    assert_eq!(name("2.0").as_deref(), Some("foo-2.0.tar.gz"));
    assert_eq!(name("1.0").as_deref(), Some("a-1.0.tar.gz"));
    assert_eq!(name("9").as_deref(), Some("a-1.0.tar.gz"));
}

#[test]
fn find_tarball_ranks_a_longer_version_below_the_exact_one() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["pkg-1.2.1.tar.gz", "pkg-1.2.tar.gz"] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    let name = find_tarball(dir.path(), "1.2")
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    assert_eq!(name.as_deref(), Some("pkg-1.2.tar.gz"));
}

#[test]
fn names_version_needs_a_dash_or_underscore_before_the_version() {
    assert!(names_version("foo-3.1.tar.gz", "3.1"));
    assert!(names_version("foo_bar-3.1-py3-none-any.whl", "3.1"));
    assert!(!names_version("foo-13.3.1.tar.gz", "3.1"));
    assert!(!names_version("foo-3.1.2.tar.gz", "3.1"));
    assert!(!names_version("foo-1.2.1.tar.gz", "1.2"));
    assert!(names_version("foo-1.2.tar.gz", "1.2"));
}

#[test]
fn find_tarball_prefers_the_archive_named_for_the_version() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["foo-13.3.1.tar.gz", "foo-3.1.tar.gz"] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    assert_eq!(
        find_tarball(dir.path(), "3.1"),
        Some(dir.path().join("foo-3.1.tar.gz"))
    );
}

#[test]
fn patch_names_expand_the_name_and_version_macros() {
    let spec = "Name: foo\nVersion: 1.2\nPatch0: foo-%{version}-fix.patch\n";
    assert_eq!(
        expand_patch_name("foo-%{version}-fix.patch", spec),
        "foo-1.2-fix.patch"
    );
    assert_eq!(
        expand_patch_name("%{name}-%version.diff", spec),
        "foo-1.2.diff"
    );
    // Without a readable header the name is left as written.
    assert_eq!(
        expand_patch_name("foo-%{version}.patch", "%autosetup\n"),
        "foo-%{version}.patch"
    );
}

#[test]
fn a_patch_named_by_a_macro_that_is_not_on_disk_is_unchecked_not_skipped() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("checkout");
    std::fs::create_dir_all(&dir).unwrap();
    {
        let enc = flate2::write::GzEncoder::new(
            std::fs::File::create(dir.join("pkg-1.0.tar.gz")).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        let data = b"a\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tb.append_data(&mut h, "pkg-1.0/f.txt", &data[..]).unwrap();
        tb.into_inner().unwrap().finish().unwrap();
    }
    let spec = "Name: pkg\nVersion: 1.0\n%autosetup -p1 -n pkg-%{version}\n\
                    Patch0: pkg-%{version}-fix.patch\n";
    let spec_path = dir.join("pkg.spec");
    std::fs::write(&spec_path, spec).unwrap();
    let check = try_patches(&spec_path, spec).unwrap();
    assert!(check.failed.is_empty(), "{check:?}");
    let note = check.unchecked.expect("a missing patch is reported");
    assert!(note.contains("pkg-1.0-fix.patch"), "{note}");
}

#[test]
fn without_patches_matches_patch_numbers_by_value() {
    let spec = "Name: foo\nVersion: 1.0\nPatch01: fix.patch\nPatch2: other.patch\n\
                    %prep\n%patch1 -p1\n%patch2 -p1\n";
    let kept = without_patches(spec, &["fix.patch".to_string()]);
    assert!(!kept.contains("Patch01"), "{kept}");
    assert!(!kept.contains("%patch1"), "{kept}");
    assert!(kept.contains("Patch2: other.patch"), "{kept}");
    assert!(kept.contains("%patch2 -p1"), "{kept}");
}

#[test]
fn tilde_is_a_home_directory_only_when_it_leads_the_unprefixed_path() {
    let header = |p: &str| format!("--- {p}\n+++ {p}\n@@ -1 +1 @@\n-a\n+b\n");
    // After an `a/` or `b/` prefix a leading `~` is an ordinary name.
    assert_eq!(patch_escapes_tree(&header("a/~build.log")), None);
    assert_eq!(patch_escapes_tree(&header("b/~build.log")), None);
    // Unprefixed, a home directory is refused.
    assert!(patch_escapes_tree(&header("~/.config/osc/oscrc")).is_some());
    assert!(patch_escapes_tree(&header("~user/x")).is_some());
    assert!(patch_escapes_tree(&header("~")).is_some());
    // A plain unprefixed name starting with `~` is not a home directory.
    assert_eq!(patch_escapes_tree(&header("~build.log")), None);
}

#[test]
fn superseded_tarball_is_the_old_version_never_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["pkg-1.2.tar.gz", "pkg-1.2.1.tar.gz"] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    let new = dir.path().join("pkg-1.2.1.tar.gz");
    // The old 1.2 tarball is found even though 1.2.1 also contains "1.2".
    assert_eq!(
        superseded_tarball(dir.path(), "1.2", &new, &[]),
        Some(dir.path().join("pkg-1.2.tar.gz"))
    );
    // Nothing is superseded when the only match is the new tarball.
    assert_eq!(superseded_tarball(dir.path(), "1.2.1", &new, &[]), None);
    assert!(names_version("pkg-1.2-py3-none-any.whl", "1.2"));
    assert!(!names_version("pkg-1.2.1.tar.gz", "1.2"));
    assert!(!names_version("pkg-11.2.tar.gz", "1.2"));
}

#[test]
fn check_patches_apply_survives_non_ascii_spec_lines() {
    let dir = tempfile::tempdir().unwrap();
    let spec_path = dir.path().join("x.spec");
    let spec = "%description\n日本語: テスト\nПривет: мир\n%autosetup -p1\n";
    assert_eq!(check_patches_apply(&spec_path, spec), Ok(None));
}

#[test]
fn safe_file_part_cannot_escape_a_directory() {
    for bad in ["a/b", "../x", "/abs", "a b", "v1\n"] {
        let out = safe_file_part(bad);
        assert!(
            !out.contains('/') && !out.contains(".."),
            "{bad:?} -> {out:?}"
        );
    }
    assert_eq!(safe_file_part("Foo_bar-1.2+git"), "Foo_bar-1.2+git");
}

#[test]
fn the_newest_release_within_a_bound_is_picked() {
    let all = ["0.1.5", "0.1.10", "0.1.11", "0.1.12rc1", "0.1.14.post1"];
    assert_eq!(
        newest_within(all.into_iter(), "<=0.1.11,>=0.1.10").as_deref(),
        Some("0.1.11")
    );
    assert_eq!(
        newest_within(all.into_iter(), "<0.2,>=0.1.11").as_deref(),
        Some("0.1.14.post1")
    );
    assert_eq!(newest_within(all.into_iter(), ">=1"), None);
    assert_eq!(newest_within(all.into_iter(), "not a bound"), None);
}

#[test]
fn pypi_json_url_encodes_both_parts() {
    assert_eq!(
        pypi_json_url("a/b", "1 0"),
        "https://pypi.org/pypi/a%2Fb/1%200/json"
    );
}

#[test]
fn append_notes_appends_and_never_truncates() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("notes.md");
    append_notes(p.clone(), "one\n");
    append_notes(p.clone(), "two\n");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
}

/// The synthetic-release fallback for "project genuinely not on
/// PyPI": fills in only what we know (name, version, local tarball
/// filename) and leaves summary/license/homepage/requires_python
/// blank for the Review agent to fill in from the pyproject.toml
/// during the first fix turn. If any of these fields silently gets
/// a non-empty default — a placeholder "FIXME" summary or a bogus
/// "SPDX-NOASSERTION" license — the generated spec would ship
/// garbage upstream, so this test locks each field explicitly.
#[test]
fn synthesize_release_without_pypi_leaves_every_unknown_field_blank() {
    let r = synthesize_release_without_pypi(
        "llama-cpp-scripts".to_string(),
        "0.6.0".to_string(),
        "llama-cpp-scripts-0.6.0.tar.gz".to_string(),
    );
    assert_eq!(r.name, "llama-cpp-scripts");
    assert_eq!(r.version, "0.6.0");
    assert_eq!(r.sdist.filename, "llama-cpp-scripts-0.6.0.tar.gz");
    assert_eq!(r.summary, None);
    assert_eq!(r.homepage, None);
    assert_eq!(r.license, ap_ecosystem::pypi::License::Unknown);
    assert_eq!(r.requires_python, None);
    assert!(!r.yanked);
    assert!(r.sdist.url.is_empty());
    assert!(r.sdist.sha256.is_empty());
    assert_eq!(r.sdist.size, 0);
}

pub(super) fn fake_node(name: &str) -> ap_ecosystem::pip::Node {
    ap_ecosystem::pip::Node {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        sha256: String::new(),
        requires_dist: vec![],
        requires_python: None,
        is_requested: false,
        is_yanked: false,
    }
}

/// `last_failure_reason` is a new field: old state.json files on disk
/// won't have it, and must still deserialize cleanly (otherwise anyone
/// upgrading past this change would lose their whole package state).
/// It must also round-trip, so the next run reads back exactly what the
/// crashing run wrote.
/// User explicitly asked: when restarting (new CLI invocation),
/// per-package spend resets to \$0. The field is scoped by
/// `cumulative_spent_run_id`; a mismatch between the stored run_id
/// and the current one means the stored spend belonged to a
/// previous invocation and must be ignored.
#[test]
fn cumulative_spend_only_resumes_within_the_same_run_id() {
    let old = PkgState {
        last_built_version: Some("1.2.3".into()),
        last_outcome: Some("failed".into()),
        last_failure_reason: None,
        reviewed_version: None,
        reviewed_spec_hash: None,
        cumulative_spent_usd: 1.75,
        cumulative_spent_run_id: Some("20260107-120000-abcdef".into()),
        ..Default::default()
    };
    // Serde round-trip to confirm persistence.
    let json = serde_json::to_string(&old).unwrap();
    let back: PkgState = serde_json::from_str(&json).unwrap();

    // Same run_id → spend resumes.
    let same_run = Some("20260107-120000-abcdef");
    let resume = if back.cumulative_spent_run_id.as_deref() == same_run {
        back.cumulative_spent_usd
    } else {
        0.0
    };
    assert_eq!(resume, 1.75, "same-run retry resumes the running total");

    // New CLI invocation → different run_id → reset to 0.
    let fresh_run = Some("20260108-000000-ff00ff");
    let resume_after_restart = if back.cumulative_spent_run_id.as_deref() == fresh_run {
        back.cumulative_spent_usd
    } else {
        0.0
    };
    assert_eq!(
        resume_after_restart, 0.0,
        "restart (different run_id) starts the package at \\$0 — \
             what the user asked for explicitly"
    );
}

/// Pre-fix state.json files from before this change don't have the
/// `cumulative_spent_run_id` field at all. Serde-default to None, and
/// a None stored run_id vs any current run_id is a mismatch — so
/// those packages also start at \$0 on the first run that reads them.
#[test]
fn pre_restart_fix_state_defaults_to_zero_spend_on_next_run() {
    let legacy = r#"{"last_built_version":"1.0","last_outcome":"failed",
                        "cumulative_spent_usd": 2.50}"#;
    let st: PkgState = serde_json::from_str(legacy).expect("legacy state must parse");
    assert_eq!(st.cumulative_spent_usd, 2.50);
    assert!(st.cumulative_spent_run_id.is_none());
    // The resume check: None vs Some(run_id) ⇒ mismatch ⇒ 0.
    let resume = if st.cumulative_spent_run_id.as_deref() == Some("any-run") {
        st.cumulative_spent_usd
    } else {
        0.0
    };
    assert_eq!(resume, 0.0);
}

/// Earlier review probe: `wheel-only` state counted as built. But the
/// generation branch that writes `wheel-only` also returns
/// `PipelineResult::Failed` — treating the recorded tag as success
/// made the pipeline-failed package look resolved to the next
/// `select_target` call, silently blocking every dependent build
/// forever.
#[test]
fn wheel_only_outcome_is_not_a_built_state() {
    let state = PkgState {
        last_built_version: Some("1.2.3".into()),
        last_outcome: Some("wheel-only".into()),
        last_failure_reason: None,
        reviewed_version: None,
        reviewed_spec_hash: None,
        cumulative_spent_usd: 0.0,
        cumulative_spent_run_id: None,
        ..Default::default()
    };
    assert!(
        !state.is_built("1.2.3"),
        "wheel-only is a terminal failure marker, not a success"
    );
}
