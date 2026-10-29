use super::*;
use crate::pipeline::checkout::*;
use crate::pipeline::obs_names::*;
use crate::pipeline::state::*;

#[test]
fn a_linked_spelling_that_differs_in_separators_is_the_same_package() {
    let listing: std::collections::HashSet<String> =
        ["python-pydantic-core", "ruamel-yaml", "python-other"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    // Found up to separators, and the classification says it is the same package.
    let shipped = linked_spelling("python-pydantic_core", &listing).unwrap();
    assert_eq!(shipped, "python-pydantic-core");
    assert_eq!(
        osc::fuzzy_key(shipped),
        osc::fuzzy_key("python-pydantic_core")
    );
    let shipped = linked_spelling("ruamel.yaml", &listing).unwrap();
    assert_eq!(shipped, "ruamel-yaml");
    assert_eq!(osc::fuzzy_key(shipped), osc::fuzzy_key("ruamel.yaml"));
    // A genuinely different name is not found at all.
    assert_eq!(linked_spelling("python-yaml", &listing), None);
    // An exact spelling wins over a variant, whatever the listing order.
    let both: std::collections::HashSet<String> = ["python-APScheduler", "python-apscheduler"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        linked_spelling("python-apscheduler", &both),
        Some("python-apscheduler")
    );
    assert_eq!(
        linked_spelling("python-APSCHEDULER", &both),
        Some("python-APScheduler")
    );
}

#[test]
fn a_dependency_key_ignores_the_python_prefix_case_and_separators() {
    for name in [
        "python-APScheduler",
        "APScheduler",
        "python-apscheduler",
        "python3-apscheduler",
    ] {
        assert_eq!(dependency_key(name), "apscheduler", "{name}");
    }
    assert_eq!(
        dependency_key("python-ruamel.yaml"),
        dependency_key("ruamel_yaml")
    );
    assert_eq!(
        dependency_key("ffmpeg-8"),
        dependency_key("python-ffmpeg-8")
    );
    assert_ne!(dependency_key("ffmpeg-8"), dependency_key("ffmpeg"));
}

#[test]
fn a_sibling_request_is_checked_under_the_text_it_is_sent_as() {
    let problem = "python-bar cannot be built: the fix belongs in python-foo";
    // The sibling records the hash of the request text it was sent.
    let recorded = vec![hash_spec(problem)];
    // The check before asking hashes the same text, so the request is over.
    assert!(patch_request_settled(&recorded, problem));
    // A different text is not the one that was sent: it does not match.
    assert!(!patch_request_settled(
        &recorded,
        "the fix belongs in python-foo"
    ));
}

#[test]
fn a_dependency_workspace_is_found_under_any_spelling_of_its_name() {
    let tmp = tempfile::tempdir().unwrap();
    // The workspace is keyed by the lowercase run name.
    std::fs::create_dir(tmp.path().join("python-apscheduler")).unwrap();
    for name in ["python-APScheduler", "APScheduler", "python-apscheduler"] {
        let ws = dependency_workspace(tmp.path(), name)
            .unwrap_or_else(|| panic!("{name} finds the workspace"));
        assert!(ws.dir().ends_with("python-apscheduler"), "{name}");
    }
    assert!(dependency_workspace(tmp.path(), "python-croniter").is_none());
    // Nothing is created by the lookup.
    assert!(!tmp.path().join("python-APScheduler").exists());
}

#[test]
fn the_obs_name_is_the_listed_spelling_of_a_case_variant_and_rpm_name_otherwise() {
    let own: std::collections::HashSet<String> = ["python-APScheduler", "python-pyyaml"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // The own project lists a case variant: that listed spelling is the OBS name.
    assert_eq!(
        resolve_obs_name("python-apscheduler", &own),
        "python-APScheduler"
    );
    // The own project lists it exactly as the pipeline names it.
    assert_eq!(resolve_obs_name("python-pyyaml", &own), "python-pyyaml");
    // Not listed yet: rpm_name. A linked project's spelling does not enter
    // here, because the branch from it is made under rpm_name.
    assert_eq!(resolve_obs_name("python-croniter", &own), "python-croniter");
}

#[test]
fn the_checkout_and_its_spec_follow_the_obs_name_in_mixed_case() {
    let (dir, spec) = checkout_paths(Path::new("/work"), "proj", "python-APScheduler");
    assert_eq!(dir, PathBuf::from("/work/proj/python-APScheduler"));
    assert_eq!(
        spec,
        PathBuf::from("/work/proj/python-APScheduler/python-APScheduler.spec")
    );
    let (lower, lower_spec) = checkout_paths(Path::new("/work"), "proj", "python-apscheduler");
    assert_eq!(lower, PathBuf::from("/work/proj/python-apscheduler"));
    assert_eq!(
        lower_spec,
        PathBuf::from("/work/proj/python-apscheduler/python-apscheduler.spec")
    );
}

#[test]
fn a_state_json_without_obs_name_loads_and_the_field_round_trips() {
    let old = r#"{"policy_version":0,"last_built_version":"3.11.0","last_outcome":"success"}"#;
    let mut st: PkgState = serde_json::from_str(old).unwrap();
    assert_eq!(st.obs_name, None);
    assert_eq!(st.last_built_version.as_deref(), Some("3.11.0"));
    st.obs_name = Some("python-APScheduler".to_string());
    let text = serde_json::to_string(&st).unwrap();
    let back: PkgState = serde_json::from_str(&text).unwrap();
    assert_eq!(back.obs_name.as_deref(), Some("python-APScheduler"));
}

#[test]
fn a_state_json_with_retired_fields_still_loads() {
    // Written by older builds: `policy_version` and an outcome that is no
    // longer produced. Unknown fields are ignored, so the package loads
    // with the fields it still has and the rest falls to the defaults.
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-old").unwrap();
    std::fs::create_dir_all(ws.dir()).unwrap();
    std::fs::write(
        ws.state_json(),
        r#"{"policy_version":0,"last_built_version":"3.11.0","last_outcome":"success","retired_field":[1,2]}"#,
    )
    .unwrap();
    let st = PkgState::try_load(&ws).unwrap();
    assert_eq!(st.last_built_version.as_deref(), Some("3.11.0"));
    assert!(st.is_built("3.11.0"));
    assert!(!ws.state_json().with_extension("json.bad").exists());
}

#[test]
fn a_release_archive_is_saved_under_the_suffix_of_its_own_kind() {
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.tar.gz"),
        Some(".tar.gz")
    );
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.tgz"),
        Some(".tar.gz")
    );
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.tar.xz"),
        Some(".tar.xz")
    );
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.txz"),
        Some(".tar.xz")
    );
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.tar.bz2"),
        Some(".tar.bz2")
    );
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.tar.zst"),
        Some(".tar.zst")
    );
    assert_eq!(archive_save_suffix("https://x/foo-1.0.zip"), Some(".zip"));
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0.tar.xz?download=1"),
        Some(".tar.xz")
    );
    // Not an archive the inspector reads: refused, not saved as .tar.gz.
    assert_eq!(archive_save_suffix("https://x/foo-1.0.tar.lz"), None);
    assert_eq!(
        archive_save_suffix("https://x/foo-1.0-py3-none-any.whl"),
        None
    );
}

#[test]
fn names_version_ends_the_version_at_an_archive_or_a_wheel_tag() {
    assert!(names_version("foo-1.2.tar.gz", "1.2"));
    assert!(names_version("foo-1.2.tgz", "1.2"));
    assert!(names_version("foo-1.2.tar.xz", "1.2"));
    assert!(names_version("foo-1.2-py3-none-any.whl", "1.2"));
    assert!(names_version("foo-1.2-cp311-cp311-linux_x86_64.whl", "1.2"));
    // A suffix that names another archive is not this version's main one.
    assert!(!names_version("foo-1.2-docs.tar.gz", "1.2"));
    assert!(!names_version("foo-1.2-data.tar.gz", "1.2"));
    assert!(!names_version("foo-1.2.post1.tar.gz", "1.2"));
}

#[test]
fn names_version_takes_any_wheel_tag_but_no_sdist_suffix() {
    // PyPy tags and build tags are wheel tags too.
    assert!(names_version(
        "foo-1.2-pp310-pypy310_pp73-linux_x86_64.whl",
        "1.2"
    ));
    assert!(names_version("foo-1.2-1-py3-none-any.whl", "1.2"));
    assert!(names_version(
        "foo-1.2-cp311-abi3-manylinux_2_17.whl",
        "1.2"
    ));
    // A suffix on an sdist still names another archive.
    assert!(!names_version("foo-1.2-docs.tar.gz", "1.2"));
    assert!(!names_version("foo-1.2-data.zip", "1.2"));
    assert!(!names_version("foo-1.2-pp310.tar.gz", "1.2"));
}

#[test]
fn a_pure_git_rename_names_its_source_and_destination() {
    let diff = "diff --git a/pkg-1.0/old.c b/pkg-1.0/new.c\n\
                    similarity index 100%\n\
                    rename from pkg-1.0/old.c\n\
                    rename to pkg-1.0/new.c\n";
    let targets = patch_targets(diff);
    for want in ["pkg-1.0/old.c", "pkg-1.0/new.c", "old.c", "new.c"] {
        assert!(targets.contains(&want.to_string()), "{want} in {targets:?}");
    }
    let copy = "diff --git a/x/src.c b/x/dst.c\ncopy from x/src.c\ncopy to x/dst.c\n";
    assert!(patch_targets(copy).contains(&"x/src.c".to_string()));
}

#[test]
fn spec_patch_level_is_the_level_of_the_line_that_applies_the_patch() {
    // `%autosetup -p0` is level 0, whatever `-p1` text the spec carries.
    let spec = "Patch0: fix.patch\n%description\nbuild with -p1 here\n%prep\n%autosetup -p0\n";
    assert_eq!(spec_patch_level(spec, "0"), 0);
    // No level on the line: the macro default, 1.
    assert_eq!(spec_patch_level("%prep\n%autosetup\n", "0"), 1);
    assert_eq!(spec_patch_level("Patch0: a.patch\n", "0"), 1);
    // A `%patchN` line gives its own level.
    let per_patch = "%prep\n%patch0 -p0\n%patch1 -p1\n";
    assert_eq!(spec_patch_level(per_patch, "0"), 0);
    assert_eq!(spec_patch_level(per_patch, "1"), 1);
    // A bare `%patch` applies patch 0.
    assert_eq!(spec_patch_level("%prep\n%patch -p0\n", "0"), 0);
}

#[test]
fn spec_patch_level_reads_every_form_of_the_patch_line() {
    // `%autosetup -N` does not apply the patch; the `%patch -P 1 -p0` line does.
    let spec = "%prep\n%autosetup -p1 -N\n%patch -P 1 -p0\n";
    assert_eq!(spec_patch_level(spec, "1"), 0);
    assert_eq!(
        spec_patch_level("%prep\n%autosetup -p1 -N\n%patch1 -p0\n", "1"),
        0
    );
    assert_eq!(
        spec_patch_level("%prep\n%autosetup -N\n%patch -p0 -P 1\n", "1"),
        0
    );
    // A `%patch -P` line with no `-p` takes the macro level, as before.
    assert_eq!(
        spec_patch_level("%prep\n%autosetup -p2\n%patch -P 1\n", "1"),
        2
    );
    // The `%autosetup` default is 1, and an explicit `-p1` is the same.
    assert_eq!(spec_patch_level("%prep\n%autosetup -p1\n", "3"), 1);
    assert_eq!(spec_patch_level("%prep\n%autosetup\n", "3"), 1);
    // A `%patch` line for another number does not give this one's level.
    assert_eq!(
        spec_patch_level("%prep\n%patch -P 2 -p0\n%autosetup -p1\n", "1"),
        1
    );
}

#[test]
fn spec_patch_level_reads_the_spaced_dash_p_as_rpm_does() {
    // rpm takes `-p 0` as well as `-p0`.
    assert_eq!(spec_patch_level("%prep\n%autosetup -p 0\n", "0"), 0);
    assert_eq!(spec_patch_level("%prep\n%autosetup -p0\n", "0"), 0);
    assert_eq!(spec_patch_level("%prep\n%patch -P 1 -p 0\n", "1"), 0);
    assert_eq!(spec_patch_level("%prep\n%patch0 -p 1\n", "0"), 1);
    // `-p` after other options.
    assert_eq!(spec_patch_level("%prep\n%autosetup -N -p 0\n", "0"), 0);
    assert_eq!(spec_patch_level("%prep\n%patch -p 0 -P 1\n", "1"), 0);
}

#[test]
fn a_companion_archive_is_not_the_main_one_unless_it_is_a_wheel() {
    assert!(!names_version("foo-1.2-cpp.tar.gz", "1.2"));
    assert!(!names_version("foo-1.2-python.tar.gz", "1.2"));
    assert!(!names_version("foo-1.2-cp.zip", "1.2"));
    // Wheel tags still match after the version.
    assert!(names_version("foo-1.2-cp311-cp311-linux_x86_64.whl", "1.2"));
    assert!(names_version("foo-1.2-py3-none-any.whl", "1.2"));
    assert!(is_main_archive("foo-1.2.tar.gz", "1.2"));
    assert!(is_main_archive("foo_1.2.zip", "1.2"));
    assert!(!is_main_archive("foo-1.2-cpp.tar.gz", "1.2"));
    assert!(!is_main_archive("foo-1.2.whl", "1.2"));
}

#[test]
fn find_tarball_prefers_the_exact_main_archive_over_a_companion() {
    let tmp = tempfile::tempdir().unwrap();
    for name in [
        "foo-1.2-cpp.tar.gz",
        "foo-1.2-python.tar.gz",
        "foo-1.2.tar.gz",
    ] {
        std::fs::write(tmp.path().join(name), b"x").unwrap();
    }
    assert_eq!(
        find_tarball(tmp.path(), "1.2")
            .unwrap()
            .file_name()
            .unwrap(),
        "foo-1.2.tar.gz"
    );
    // A wheel of the version is still a match when no sdist is there.
    let wheels = tempfile::tempdir().unwrap();
    std::fs::write(wheels.path().join("foo-1.2-py3-none-any.whl"), b"x").unwrap();
    assert_eq!(
        find_tarball(wheels.path(), "1.2")
            .unwrap()
            .file_name()
            .unwrap(),
        "foo-1.2-py3-none-any.whl"
    );
}

#[test]
fn stale_patch_prefix_reads_prerelease_and_local_release_directories() {
    let judged = |first: &str| {
        let diff = format!("--- a/{first}/x.py\n+++ b/{first}/x.py\n");
        stale_patch_prefix(&diff, "python-torch-2.14", "2.14")
    };
    // Pre-release, pre-release number, and local versions are other releases.
    assert_eq!(judged("foo-1.2rc1").as_deref(), Some("foo-1.2rc1"));
    assert_eq!(
        judged("python-torch-2.4.0a0").as_deref(),
        Some("python-torch-2.4.0a0")
    );
    assert_eq!(
        judged("foo-1.2+git2024").as_deref(),
        Some("foo-1.2+git2024")
    );
    assert_eq!(judged("foo-2.13.post1").as_deref(), Some("foo-2.13.post1"));
    // The current version, the top-level directory, and a plain `src/` are not stale.
    assert_eq!(judged("python-torch-2.14"), None);
    assert_eq!(
        judged("python-torch-2.14rc1").as_deref(),
        Some("python-torch-2.14rc1")
    );
    assert_eq!(judged("src"), None);
    // A directory whose tail is not a version is not a release.
    assert_eq!(judged("foo-bar"), None);
    assert_eq!(judged("foo-1.2-extra"), None);
}

#[test]
fn stale_patch_prefix_judges_a_git_style_name_by_its_release_directory() {
    let diff = "--- a/foo-1.0/src/x.c\n+++ b/foo-1.0/src/x.c\n";
    assert_eq!(
        stale_patch_prefix(diff, "foo-2.0", "2.0").as_deref(),
        Some("foo-1.0")
    );
    // The right directory, under the git prefix, fits.
    let fine = "--- a/foo-2.0/src/x.c\n+++ b/foo-2.0/src/x.c\n";
    assert_eq!(stale_patch_prefix(fine, "foo-2.0", "2.0"), None);
}

#[test]
fn find_tarball_prefers_the_main_archive_over_a_docs_one() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["foo-1.2-docs.tar.gz", "foo-1.2.tar.gz"] {
        std::fs::write(dir.path().join(f), b"").unwrap();
    }
    assert_eq!(
        find_tarball(dir.path(), "1.2")
            .unwrap()
            .file_name()
            .unwrap(),
        "foo-1.2.tar.gz"
    );
    // A .tgz and a .tar.xz are archives too.
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("foo-1.2.tgz"), b"").unwrap();
    assert!(find_tarball(other.path(), "1.2").is_some());
    let xz = tempfile::tempdir().unwrap();
    std::fs::write(xz.path().join("foo-1.2.tar.xz"), b"").unwrap();
    assert!(find_tarball(xz.path(), "1.2").is_some());
}

#[test]
fn a_malformed_hand_written_patch_is_caught_before_the_build() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    // pristine tarball with one file
    let tarball = dir.path().join("pkg-1.0.tar.gz");
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
    let spec = "%autosetup -p1 -n pkg-%{version}\nPatch0:         fix.patch\n";
    let spec_path = dir.path().join("pkg.spec");
    std::fs::write(&spec_path, spec).unwrap();

    // hunk header says 5 lines, body has 3: the shape hand-written patches get wrong
    std::fs::write(
        dir.path().join("fix.patch"),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n",
    )
    .unwrap();
    assert!(check_patches_apply(&spec_path, spec).is_err());

    let mut good = std::fs::File::create(dir.path().join("fix.patch")).unwrap();
    write!(
        good,
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
    )
    .unwrap();
    drop(good);
    assert_eq!(check_patches_apply(&spec_path, spec), Ok(None));

    // A source archive that will not unpack is not a patch failure, but the
    // patches went unchecked and that is reported rather than skipped.
    std::fs::write(&tarball, b"not a gzip stream").unwrap();
    let unchecked = check_patches_apply(&spec_path, spec)
        .unwrap()
        .expect("an unpack failure is reported");
    assert!(unchecked.contains("were not checked"), "{unchecked}");

    // nothing to check without patches
    assert!(check_patches_apply(&spec_path, "%autosetup -p1\n").is_ok());
}

#[test]
fn patch_escapes_tree_rejects_absolute_home_and_dotdot_headers() {
    let abs = "--- a/etc/passwd\n+++ /home/u/.config/osc/oscrc\n@@ -1 +1 @@\n-a\n+b\n";
    let got = patch_escapes_tree(abs).expect("absolute path");
    assert!(got.contains("/home/u/.config/osc/oscrc"), "{got}");
    assert!(patch_escapes_tree("--- /etc/x\n+++ /etc/x\n").is_some());
    assert!(patch_escapes_tree("--- a/../../x\n+++ b/../../x\n").is_some());
    assert!(patch_escapes_tree("--- ../x\n+++ ../x\n").is_some());
    assert!(patch_escapes_tree("--- a/src/../../x\t2020-01-01\n+++ b/x\n").is_some());
    assert!(patch_escapes_tree("--- ~/.ssh/id\n+++ ~/.ssh/id\n").is_some());
    assert!(patch_escapes_tree("diff --git a/src/x b/../../y\n").is_some());
    assert!(patch_escapes_tree("Index: /etc/shadow\n").is_some());
    assert!(patch_escapes_tree("rename from ../a\nrename to b\n").is_some());
}

#[test]
fn patch_escapes_tree_allows_dev_null_and_normal_paths() {
    assert_eq!(
        patch_escapes_tree("--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+x\n"),
        None
    );
    assert_eq!(
        patch_escapes_tree("--- a/src/x.c\t2020-01-01 00:00:00\n+++ b/src/x.c\n"),
        None
    );
    assert_eq!(
        patch_escapes_tree("diff --git a/src/x.c b/src/x.c\n--- a/src/x.c\n+++ b/src/x.c\n"),
        None
    );
    // A removed line that happens to start with `-- ` is a hunk line, not a header.
    assert_eq!(
        patch_escapes_tree("--- a/x.sql\n+++ b/x.sql\n@@ -1 +0,0 @@\n--- /etc/x\n"),
        None
    );
}

#[test]
fn patch_escapes_tree_reads_a_space_separated_dev_null_timestamp() {
    assert_eq!(
        patch_escapes_tree(
            "--- /dev/null 1970-01-01 00:00:00 +0000\n+++ b/new.txt\n@@ -0,0 +1 @@\n+x\n"
        ),
        None
    );
}

#[test]
fn patch_escapes_tree_reads_context_diff_headers() {
    let escaping = "*** ../../x\t2020-01-01\n--- ../../x\t2020-01-01\n***************\n*** 1 ****\n! a\n--- 1 ----\n! b\n";
    assert!(patch_escapes_tree(escaping).is_some());
    let inside = "*** a/src/x.c\t2020-01-01\n--- b/src/x.c\t2020-01-01\n***************\n*** 1,3 ****\n--- 1,3 ----\n";
    assert_eq!(patch_escapes_tree(inside), None);
}

#[test]
fn pre_existing_absolute_header_patch_is_not_the_turns_edit() {
    let dir = tempfile::tempdir().unwrap();
    let spec_path = dir.path().join("pkg.spec");
    let spec = "Name: pkg\nPatch0: old.patch\n";
    std::fs::write(&spec_path, spec).unwrap();
    let old = "--- /tmp/orig/x\n+++ /tmp/new/x\n@@ -1 +1 @@\n-a\n+b\n";
    std::fs::write(dir.path().join("old.patch"), old).unwrap();

    // Untouched since the pre-turn snapshot: not rejected.
    let snap = CheckoutSnapshot::take(dir.path());
    assert_eq!(
        patch_escape_problem(&spec_path, spec, &PatchBaseline::Snapshot(&snap)),
        None
    );
    // OBS already publishes it: not judged at commit.
    let published = PatchBaseline::NotPublished(vec!["old.patch".to_string()]);
    assert_eq!(patch_escape_problem(&spec_path, spec, &published), None);
    // With no pre-turn copy, a patch OBS publishes is not judged either.
    let no_baseline = PatchBaseline::NoBaseline(vec!["old.patch".to_string()]);
    assert_eq!(patch_escape_problem(&spec_path, spec, &no_baseline), None);
}
