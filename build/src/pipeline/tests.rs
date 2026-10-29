mod part2;
mod part3;
mod part4;
mod part5;
use part3::*;

#[test]
fn stale_failure_state_needs_rpm_reconcile() {
    let failed = PkgState {
        last_outcome: Some("failed".into()),
        last_failure_reason: Some("OBS: package source is broken".into()),
        ..PkgState::default()
    };
    assert!(state_needs_rpm_reconcile(&failed));
    let ok = PkgState {
        last_outcome: Some("success".into()),
        ..PkgState::default()
    };
    assert!(!state_needs_rpm_reconcile(&ok));
}

use super::*;
use crate::pipeline::licence::*;

#[test]
fn the_changelog_has_no_macros_for_the_packages_own_name_and_version() {
    assert_eq!(
        without_macros(
            "Move the runtime library to lib%{name}1_4 (%{version})",
            "aws-c-s3",
            "1.4.1"
        ),
        "Move the runtime library to libaws-c-s31_4 (1.4.1)"
    );
}

#[test]
fn the_service_revision_moves_to_the_new_tag() {
    let svc = "<services><service name=\"tar_scm\"><param name=\"revision\">v1.1.2</param><param name=\"url\">u</param></service></services>";
    assert_eq!(
        with_service_revision(svc, "v1.3.7").unwrap(),
        "<services><service name=\"tar_scm\"><param name=\"revision\">v1.3.7</param><param name=\"url\">u</param></service></services>"
    );
    assert!(with_service_revision("<services/>", "v1").is_none());
}

#[test]
fn the_commit_message_summarises_the_new_changelog_entry() {
    let sep = "-".repeat(67);
    let old = format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Old\n");
    let new = format!(
        "{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 2.0\n- Drop the obsolete fix.patch\n  and more\n\n"
    );
    assert_eq!(
        changes_summary(&format!("{new}{old}"), &old).unwrap(),
        "Update to 2.0; Drop the obsolete fix.patch and more"
    );
    let both = format!("{new}{old}");
    assert!(changes_summary(&both, &both).is_none());
    let long = format!(
        "{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- {}\n",
        "word ".repeat(40)
    );
    assert!(changes_summary(&long, "").unwrap().chars().count() <= 100);
}

#[test]
fn narration_is_not_a_changelog_bullet() {
    assert!(is_narration(
        "The build still installed aws-c-http-devel-0.11.0 despite the requirement"
    ));
    assert!(is_narration("All 9 tests failed with an option error"));
    assert!(is_narration("Pre-build review of influxdb-cxx"));
    assert!(!is_narration("Drop the obsolete fix-cmake.patch"));
    assert!(!is_narration("Replace %make_jobs with %cmake_build"));
}

#[test]
fn a_trailing_file_list_is_not_in_the_changelog() {
    assert_eq!(
        without_file_list(
            "Drop the patch, which upstream already contains (src/fa.c, fa.h, tests/fatest.c)"
        ),
        "Drop the patch, which upstream already contains"
    );
    let keep = "Build with ninja (the default generator)";
    assert_eq!(without_file_list(keep), keep);
}

#[test]
fn the_packages_own_version_is_left_to_the_update_entry() {
    assert_eq!(
        without_own_version(
            "Raise the cmake minimum to 3.16 to match cmake_minimum_required in Catch2 3.16.1",
            "Catch2",
            "3.16.1"
        ),
        "Raise the cmake minimum to 3.16 to match cmake_minimum_required"
    );
    assert_eq!(
        without_own_version(
            "Add the CPS directory for CRCpp 1.2.3.0",
            "CRCpp",
            "1.2.3.0"
        ),
        "Add the CPS directory"
    );
    // Another version, or the version mid-sentence, is kept.
    let keep = "Raise the cmake minimum to 3.16";
    assert_eq!(without_own_version(keep, "Catch2", "3.16.1"), keep);
    let mid = "Build 3.16.1 with ninja";
    assert_eq!(without_own_version(mid, "Catch2", "3.16.1"), mid);
}

#[test]
fn changes_message_keeps_one_bullet_per_line() {
    assert_eq!(
        changes_message("Relax the numpy bound.\nDrop the old pin. More text."),
        "Relax the numpy bound\nDrop the old pin"
    );
    assert_eq!(
        changes_message("- Add a.patch to fix x\n- Drop y"),
        "Add a.patch to fix x\nDrop y"
    );
    assert_eq!(
        changes_message("The build log showed it"),
        "Fix the packaging"
    );
    assert_eq!(changes_message("   "), "Update the packaging");
    assert!(!changes_message(&"word ".repeat(100)).ends_with("..."));
    assert!(changes_message(&"word ".repeat(100)).chars().count() <= 120);
}

#[test]
fn a_long_changes_message_is_cut_at_a_clause_without_an_ellipsis() {
    let m = changes_message(
        "Pass the tests the sdist does not ship through the deselect list, and its cmake \
             flags keep the build from reaching the network at all",
    );
    assert_eq!(
        m,
        "Pass the tests the sdist does not ship through the deselect list"
    );
    assert_eq!(
        changes_message("Escape %%{version} in the macro"),
        "Escape %{version} in the macro"
    );
}

#[test]
fn the_structured_output_nudge_says_not_to_redo_the_work() {
    assert!(STRUCTURED_OUTPUT_NUDGE.contains("already made"));
    assert!(STRUCTURED_OUTPUT_NUDGE.contains("on disk"));
    assert!(STRUCTURED_OUTPUT_NUDGE.contains("Do NOT"));
    assert!(STRUCTURED_OUTPUT_NUDGE.contains("StructuredOutput call"));
}

#[test]
fn max_turns_is_reported_as_its_own_failure_not_a_format_problem() {
    let e = crate::agent::Error::EndedEarly(Box::new(crate::agent::EndedEarlyError {
        subtype: "error_max_turns".into(),
        message: "error_max_turns".into(),
        stdout: String::new(),
        prompt: String::new(),
        usage: Default::default(),
    }));
    let msg = friendly_agent_error(&e, 2.0, "python-foo");
    assert!(msg.contains("ran out of turns"), "{msg}");
}

#[test]
fn validate_path_component_rejects_unsafe_names() {
    for bad in ["", ".", "..", "a/b", "../etc", ".hidden", "x..y", "/abs"] {
        assert!(
            validate_path_component("name", bad).is_err(),
            "{bad:?} should be rejected"
        );
    }
    for good in ["python-requests", "openSUSE:Factory", "home:user_1.2"] {
        assert!(
            validate_path_component("name", good).is_ok(),
            "{good:?} should be accepted"
        );
    }
}

#[test]
fn pkg_state_survives_a_round_trip_and_leaves_no_temp_file() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    let st = PkgState {
        last_built_version: Some("1.2.3".into()),
        last_outcome: Some("success".into()),
        ..Default::default()
    };
    st.save(&ws).unwrap();
    let loaded = PkgState::load(&ws);
    assert_eq!(loaded.last_built_version.as_deref(), Some("1.2.3"));
    assert!(!ws.state_json().with_extension("json.tmp").exists());
}

/// A state file that cannot be read must fail an update, not be replaced
/// by the default state (which would reset the budget counters on save).
#[test]
fn update_fails_instead_of_overwriting_an_unreadable_state_file() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    // A directory where the file should be: reading it fails with a
    // non-NotFound error.
    std::fs::create_dir_all(ws.state_json()).unwrap();
    assert!(PkgState::try_load(&ws).is_err());
    assert!(PkgState::update(&ws, |s| s.cumulative_attempts = 1).is_err());
    assert!(
        ws.state_json().is_dir(),
        "the unreadable entry must be left alone"
    );
    // A missing file is still the default state.
    std::fs::remove_dir(ws.state_json()).unwrap();
    assert_eq!(PkgState::try_load(&ws).unwrap().cumulative_attempts, 0);
}

/// The persisted spec hash is SHA-256, not `DefaultHasher`, so it does not
/// change between Rust releases: this pins the value.
#[test]
fn hash_spec_is_the_first_eight_bytes_of_sha256() {
    // SHA-256("abc") = ba7816bf 8f01cfea 414140de 5dae2223 ...
    assert_eq!(hash_spec("abc"), 0xba78_16bf_8f01_cfea);
}

/// Two writers each doing 50 read-modify-writes must not lose any: without
/// the lock, both load the same count and the later save erases the other.
#[test]
fn concurrent_updates_do_not_lose_writes() {
    let root = tempfile::tempdir().unwrap();
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = root.path().to_path_buf();
            std::thread::spawn(move || {
                let ws = Workspace::open(&path, "python-foo").unwrap();
                for _ in 0..50 {
                    PkgState::update(&ws, |st| st.cumulative_attempts += 1).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    assert_eq!(PkgState::load(&ws).cumulative_attempts, 100);
}

/// A truncated or corrupt state.json must be kept aside, not replaced by
/// the default on the next save, which would erase the package's history.
#[test]
fn a_corrupt_state_file_is_preserved_not_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    std::fs::write(ws.state_json(), "{ \"last_built_version\": \"1.0\", ").unwrap();
    let loaded = PkgState::load(&ws);
    assert!(loaded.last_built_version.is_none());
    let kept = ws.state_json().with_extension("json.bad");
    assert!(std::fs::read_to_string(kept).unwrap().contains("1.0"));
}

/// A state.json that cannot be read (here: it is a directory) is not a
/// corrupt file. It must not be moved aside as if it were, and the load
/// still falls back to the default instead of failing the run.
#[test]
fn an_unreadable_state_file_is_left_in_place() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    std::fs::create_dir(ws.state_json()).unwrap();
    let loaded = PkgState::load(&ws);
    assert!(loaded.last_built_version.is_none());
    assert!(ws.state_json().is_dir());
    assert!(!ws.state_json().with_extension("json.bad").exists());
}

/// A failed save must say which file it could not write.
#[test]
fn a_failed_state_save_names_the_file() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    std::fs::remove_dir_all(ws.dir()).unwrap();
    let err = PkgState::default().save(&ws).unwrap_err();
    assert!(err.to_string().contains("state.json"), "{err}");
}

#[test]
fn a_missing_state_file_is_just_the_default() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    assert!(PkgState::load(&ws).last_built_version.is_none());
    assert!(!ws.state_json().with_extension("json.bad").exists());
}

#[test]
fn an_invalid_agent_jobs_value_is_never_unlimited() {
    assert_eq!(parse_agent_jobs(None), (None, true));
    assert_eq!(parse_agent_jobs(Some("")), (None, true));
    assert_eq!(parse_agent_jobs(Some("3")), (Some(3), true));
    for junk in ["0", "-1", "many", "1.5"] {
        assert_eq!(
            parse_agent_jobs(Some(junk)),
            (Some(DEFAULT_AGENT_JOBS), false),
            "{junk:?}"
        );
    }
}

#[test]
fn agent_turns_are_uncapped_unless_asked() {
    // SAFETY: tests in this module that touch this variable run serially
    // through the lock below.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::remove_var("AUTO_PACKAGE_AGENT_JOBS") };
    assert_eq!(AgentSlot::limit(), None);
    for junk in ["0", "-1", "many"] {
        unsafe { std::env::set_var("AUTO_PACKAGE_AGENT_JOBS", junk) };
        assert_eq!(AgentSlot::limit(), Some(DEFAULT_AGENT_JOBS), "{junk:?}");
    }
    unsafe { std::env::set_var("AUTO_PACKAGE_AGENT_JOBS", "7") };
    assert_eq!(AgentSlot::limit(), Some(7));
    unsafe { std::env::remove_var("AUTO_PACKAGE_AGENT_JOBS") };
}

/// The service-run gate trusts `missing_source_files`: a URL Source is judged
/// by the file name it would download to.
#[test]
fn a_url_source_is_present_once_its_file_is_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let spec = "Source0: https://files.pythonhosted.org/packages/ab/cd/x-1.0-cp313-cp313-manylinux_2_27_x86_64.whl\n";
    assert_eq!(
        missing_source_files(spec, dir.path()),
        vec!["x-1.0-cp313-cp313-manylinux_2_27_x86_64.whl"]
    );
    std::fs::write(
        dir.path()
            .join("x-1.0-cp313-cp313-manylinux_2_27_x86_64.whl"),
        b"",
    )
    .unwrap();
    assert!(missing_source_files(spec, dir.path()).is_empty());
}

#[test]
fn maturin_service_vendors_the_crate_maturin_builds() {
    let plain = render_maturin_service_file("outlines_core", &[], false, true);
    assert_eq!(plain.matches("name=\"cargotoml\"").count(), 1);
    let xml =
        render_maturin_service_file("hf_xet", &["hf_xet/Cargo.toml".to_string()], false, true);
    assert!(
        xml.contains("<param name=\"cargotoml\">Cargo.toml</param>"),
        "{xml}"
    );
    assert!(
        xml.contains("<param name=\"cargotoml\">hf_xet/Cargo.toml</param>"),
        "{xml}"
    );
}

#[test]
fn maturin_service_omits_a_root_manifest_the_sdist_lacks() {
    let xml = render_maturin_service_file(
        "safetensors",
        &["bindings/python/Cargo.toml".to_string()],
        false,
        false,
    );
    assert_eq!(xml.matches("name=\"cargotoml\"").count(), 1, "{xml}");
    assert!(!xml.contains(">Cargo.toml<"), "{xml}");
}

#[test]
fn a_locked_crate_is_vendored_without_cargo_update() {
    let locked = render_maturin_service_file("chromadb", &[], true, true);
    assert!(
        locked.contains("<param name=\"update\">false</param>"),
        "{locked}"
    );
    let unlocked = render_maturin_service_file("chromadb", &[], false, true);
    assert!(
        unlocked.contains("<param name=\"update\">true</param>"),
        "{unlocked}"
    );
}

#[test]
fn service_files_escape_values_taken_from_upstream() {
    let xml = render_maturin_service_file(
        "x",
        &["a&b</param><evil/>/Cargo.toml".to_string()],
        false,
        true,
    );
    assert!(!xml.contains("<evil/>"), "{xml}");
    assert!(xml.contains("a&amp;b&lt;/param&gt;"), "{xml}");
    let xml = render_service_file("https://example.org/o/r?a=1&b=2", "1.0", "r");
    assert!(xml.contains("a=1&amp;b=2.git"), "{xml}");
}

#[test]
fn maturin_manifest_path_is_read_from_pyproject() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("hf_xet-1.7.0.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            std::fs::File::create(&tarball).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        let data = b"[build-system]\nrequires=[\"maturin\"]\nbuild-backend=\"maturin\"\n[tool.maturin]\nmanifest-path = \"hf_xet/Cargo.toml\"\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tb.append_data(&mut h, "hf_xet-1.7.0/pyproject.toml", &data[..])
            .unwrap();
        tb.into_inner().unwrap().finish().unwrap();
    }
    assert_eq!(
        maturin_manifest_path(&tarball).unwrap().as_deref(),
        Some("hf_xet/Cargo.toml")
    );
}

/// `chromadb`'s `pyproject.toml` points maturin at `rust/python_bindings/Cargo.toml`,
/// but that crate is a member of the workspace rooted at the archive's own
/// `Cargo.toml` — vendoring it as a second, separate `cargotoml` param makes
/// `obs-service-cargo` fail with a bare `NotFound` looking for a lockfile
/// next to the member instead of at the workspace root it actually lives in.
#[test]
fn maturin_manifest_path_skips_a_root_workspace_member() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("chromadb-1.5.9.tar.gz");
    {
        let enc = flate2::write::GzEncoder::new(
            std::fs::File::create(&tarball).unwrap(),
            flate2::Compression::default(),
        );
        let mut tb = tar::Builder::new(enc);
        let mut add = |name: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tb.append_data(&mut h, name, data).unwrap();
        };
        add(
            "chromadb-1.5.9/pyproject.toml",
            b"[build-system]\nrequires=[\"maturin\"]\nbuild-backend=\"maturin\"\n[tool.maturin]\nmanifest-path = \"rust/python_bindings/Cargo.toml\"\n",
        );
        add(
            "chromadb-1.5.9/Cargo.toml",
            b"[workspace]\nmembers = [\"rust/python_bindings\", \"rust/types\"]\n",
        );
        tb.into_inner().unwrap().finish().unwrap();
    }
    assert_eq!(maturin_manifest_path(&tarball), Ok(None));
}

/// An archive that cannot be unpacked is an error to report, not the same
/// answer as "no manifest path".
#[test]
fn maturin_manifest_path_reports_an_archive_it_cannot_unpack() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = dir.path().join("broken-1.0.tar.gz");
    std::fs::write(&tarball, b"not a gzip stream").unwrap();
    assert!(maturin_manifest_path(&tarball).is_err());
}

#[test]
fn patch_targets_are_the_diff_names_minus_the_first_component() {
    let diff = "--- a/src/x.c\t2026-01-01\n+++ b/src/x.c\n--- /dev/null\n+++ b/new.h\n";
    assert_eq!(patch_targets(diff), vec!["src/x.c", "src/x.c", "new.h"]);
    // A name with no directory part is its own target under -p1.
    let bare = "--- setup.py.orig\n+++ setup.py\n";
    assert_eq!(patch_targets(bare), vec!["setup.py.orig", "setup.py"]);
}

#[test]
fn own_name_matches_a_package_up_to_case_and_separators() {
    let own: std::collections::HashSet<String> = ["python-APScheduler", "python-pyyaml"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        own_name("python-APScheduler", &own),
        OwnName::Exact,
        "the exact spelling"
    );
    assert_eq!(
        own_name("python-apscheduler", &own),
        OwnName::Variant("python-APScheduler"),
        "a case variant is reported, not taken as the pipeline's name"
    );
    assert_eq!(
        own_name("python-pyyaml", &own),
        OwnName::Exact,
        "the lowercase spelling is exact here"
    );
    assert_eq!(own_name("python-yaml", &own), OwnName::Absent);
    // With both spellings present the exact one wins, whatever the order.
    let both: std::collections::HashSet<String> = ["python-APScheduler", "python-apscheduler"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(own_name("python-apscheduler", &both), OwnName::Exact);
    assert_eq!(
        own_name("python-APSCHEDULER", &both),
        OwnName::Variant("python-APScheduler")
    );
}

#[test]
fn preferred_own_spelling_is_the_same_whatever_the_listing_order() {
    let a = "python-APScheduler".to_string();
    let b = "python-apscheduler".to_string();
    // The lowercase rpm name wins when listed, in either order.
    assert_eq!(
        preferred_own_spelling("python-apscheduler", &[&a, &b]).map(String::as_str),
        Some("python-apscheduler")
    );
    assert_eq!(
        preferred_own_spelling("python-apscheduler", &[&b, &a]).map(String::as_str),
        Some("python-apscheduler")
    );
    // Without the lowercase spelling, the smallest one, in either order.
    let only_upper = "python-APScheduler".to_string();
    let other = "python-APSCHEDULER".to_string();
    assert_eq!(
        preferred_own_spelling("python-apscheduler", &[&only_upper, &other]).map(String::as_str),
        Some("python-APSCHEDULER")
    );
    assert_eq!(
        preferred_own_spelling("python-apscheduler", &[&other, &only_upper]).map(String::as_str),
        Some("python-APSCHEDULER")
    );
    assert_eq!(preferred_own_spelling("python-apscheduler", &[]), None);
}
