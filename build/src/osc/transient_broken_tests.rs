use super::*;

#[test]
fn broken_needs_a_nonempty_log_to_be_confirmed() {
    let dir = std::env::temp_dir().join(format!("ap-broken-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("build.log");
    assert!(!broken_is_confirmed(&log), "missing log");
    fs::write(&log, "\n  \n").unwrap();
    assert!(!broken_is_confirmed(&log), "empty log");
    fs::write(&log, "[1s] error: bad source\n").unwrap();
    assert!(broken_is_confirmed(&log));
    fs::remove_dir_all(&dir).ok();
}

fn emit_quiet() -> Emit<'static> {
    Emit::quiet()
}

#[test]
fn an_unconfirmed_broken_is_re_triggered_until_it_builds() {
    let mut builds = 0;
    let mut rebuilds = 0;
    let r = build_with_retriggers(
        "python-x",
        emit_quiet(),
        Duration::ZERO,
        || {
            builds += 1;
            if builds < 3 {
                Err(Error::NotActionable(BROKEN_UNCONFIRMED.to_string()))
            } else {
                Ok(())
            }
        },
        || {
            rebuilds += 1;
            Ok(())
        },
    );
    assert!(r.is_ok());
    assert_eq!((builds, rebuilds), (3, 2));
}

#[test]
fn an_unconfirmed_broken_that_persists_stands_as_broken_after_the_bound() {
    let mut builds = 0;
    let r = build_with_retriggers(
        "python-x",
        emit_quiet(),
        Duration::ZERO,
        || {
            builds += 1;
            Err(Error::NotActionable(BROKEN_UNCONFIRMED.to_string()))
        },
        // A failing re-trigger only warns; the loop still ends.
        || Err(Error::NotActionable("rebuild refused".into())),
    );
    assert_eq!(builds, MAX_BROKEN_RETRIGGERS + 1);
    match r {
        Err(Error::NotActionable(m)) => assert_eq!(m, BROKEN_MESSAGE),
        other => panic!("{other:?}"),
    }
}

#[test]
fn any_other_result_is_returned_without_a_retrigger() {
    for (expect_ok, result) in [
        (
            false,
            Err(Error::NotActionable("OBS: build is disabled".into())),
        ),
        (false, Err(Error::NotActionable(BROKEN_MESSAGE.into()))),
        (true, Ok(())),
    ] {
        let mut rebuilds = 0;
        let mut result = Some(result);
        let r = build_with_retriggers(
            "python-x",
            emit_quiet(),
            Duration::ZERO,
            || result.take().unwrap(),
            || {
                rebuilds += 1;
                Ok(())
            },
        );
        assert_eq!(rebuilds, 0);
        assert_eq!(r.is_ok(), expect_ok);
    }
}

#[test]
fn getbinaries_is_retried_and_a_late_success_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut calls = 0;
    let r = fetch_with_retries(
        "p",
        "python-x",
        "r",
        "x86_64",
        dir.path(),
        emit_quiet(),
        4,
        Duration::ZERO,
        || {
            calls += 1;
            if calls < 3 {
                Err(Error::OscFailed {
                    cmd: "getbinaries".into(),
                    code: 1,
                    stderr: "502".into(),
                })
            } else {
                Ok(())
            }
        },
    );
    assert!(r.is_ok());
    assert_eq!(calls, 3);
}

#[test]
fn getbinaries_gives_up_after_its_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let mut calls = 0;
    let r = fetch_with_retries(
        "p",
        "python-x",
        "r",
        "x86_64",
        dir.path(),
        emit_quiet(),
        4,
        Duration::ZERO,
        || {
            calls += 1;
            Err(Error::OscFailed {
                cmd: "getbinaries".into(),
                code: 1,
                stderr: "502".into(),
            })
        },
    );
    assert!(matches!(r, Err(Error::OscFailed { code: 1, .. })));
    assert_eq!(calls, 4);
}

#[test]
fn an_unpublished_update_entry_for_another_version_is_dropped() {
    let sep = "-".repeat(67);
    let old = format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.1.1\n");
    let wrong = format!("{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 0.0.1\n\n");
    let working = format!("{wrong}{old}");
    assert_eq!(
        without_unpublished_update_entry(&working, &old, "Update to 1.1.1").unwrap(),
        old
    );
    // Already published, the same version, or not an update: kept.
    assert!(without_unpublished_update_entry(&working, &working, "Update to 1.1.1").is_none());
    assert!(without_unpublished_update_entry(&working, &old, "Update to 0.0.1").is_none());
    assert!(without_unpublished_update_entry(&working, &old, "Initial packaging").is_none());
}

#[test]
fn a_long_changelog_line_is_wrapped_under_its_bullet() {
    let sep = "-".repeat(67);
    let long = "- Drop the obsolete fix-cmake.patch and add the CPS share directory to the devel package for CRCpp 1.2.3.0";
    let working = format!(
        "{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n{long}\n\n{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Old\n"
    );
    let pristine = format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Old\n");
    let out = wrapped_newest_entry(&working, &pristine).unwrap();
    assert!(out.contains("- Drop the obsolete fix-cmake.patch\n- Add the CPS share directory to the devel package for CRCpp\n  1.2.3.0\n"), "{out}");
    // No second verb: wrapped under the bullet instead.
    let one = "- Drop the obsolete fix-cmake.patch and the CPS share directory from the devel package of CRCpp";
    assert_eq!(wrap_changes_line(one).len(), 2);
    assert!(wrap_changes_line(one)[1].starts_with("  "));
    assert!(out.lines().all(|l| l.chars().count() <= 67));
    // A published entry is left alone.
    assert!(wrapped_newest_entry(&working, &working).is_none());
}

#[test]
fn a_fix_joins_the_release_entry_instead_of_opening_another() {
    let sep = "-".repeat(67);
    let old = format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.0\n");
    let new = format!("{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 2.0\n\n");
    let working = format!("{new}{old}");
    let out = with_bullet_in_release_entry(&working, "2.0", "Drop fix.patch").unwrap();
    assert!(
        out.contains("- Update to 2.0\n- Drop fix.patch\n\n"),
        "{out}"
    );
    assert!(out.ends_with(&old));
    // The same bullet twice is kept once.
    assert_eq!(
        with_bullet_in_release_entry(&out, "2.0", "Drop fix.patch").unwrap(),
        out
    );
    // An earlier release's entry is never added to.
    assert!(with_bullet_in_release_entry(&working, "1.0", "x").is_none());
    assert!(with_bullet_in_release_entry(&old, "2.0", "x").is_none());
}

#[test]
fn flag_lines_go_under_the_release_bullet_and_replace_stale_ones() {
    let sep = "-".repeat(67);
    let old = format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.0\n");
    let new = format!(
        "{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 2.0\n- Fix the build\n\n"
    );
    let working = format!("{new}{old}");
    let flags = vec!["FLAG: tests skipped: --deselect t.py::a".to_string()];
    let out = with_flag_lines_in_release_entry(&working, "2.0", &flags).unwrap();
    assert!(
        out.contains(
            "- Update to 2.0\n  FLAG: tests skipped: --deselect t.py::a\n- Fix the build\n\n"
        ),
        "{out}"
    );
    assert!(out.ends_with(&old));
    // Setting the same flags again changes nothing; a cleared set removes them.
    assert_eq!(
        with_flag_lines_in_release_entry(&out, "2.0", &flags).unwrap(),
        out
    );
    let cleared = with_flag_lines_in_release_entry(&out, "2.0", &[]).unwrap();
    assert_eq!(cleared, working);
    // An earlier release's entry is never written to.
    assert!(with_flag_lines_in_release_entry(&working, "1.0", &flags).is_none());
}

#[test]
fn a_generated_commit_list_collapses_to_the_update_line() {
    let sep = "-".repeat(67);
    let old = format!("{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 1.0\n");
    let bullets: String = (0..8).map(|i| format!("  * change {i} (#{i})\n")).collect();
    let new = format!(
        "{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to version 1.3.7:\n{bullets}\n"
    );
    let out = collapsed_generated_entry(&format!("{new}{old}"), &old).unwrap();
    assert!(out.contains("- Update to 1.3.7\n\n"), "{out}");
    assert!(!out.contains("change 3"));
    assert!(out.ends_with(&old));
    // A short list, or one OBS holds, stays.
    let short = format!(
        "{sep}\nTue Jan  2 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to version 1.3.7:\n  * one\n\n"
    );
    assert!(collapsed_generated_entry(&format!("{short}{old}"), &old).is_none());
    let both = format!("{new}{old}");
    assert!(collapsed_generated_entry(&both, &both).is_none());
}

#[test]
fn a_noarch_subpackage_does_not_make_constraints_unused() {
    let spec = "Name: m\nVersion: 1\nBuildRequires: gcc\n%description\nx\n%package test\nBuildArch: noarch\n";
    assert!(!recipe_file_unused("_constraints", spec));
    let noarch = "Name: m\nBuildArch: noarch\n%description\nx\n";
    assert!(recipe_file_unused("_constraints", noarch));
}

#[test]
fn a_branch_links_back_to_the_package_it_was_made_from() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(link_target(dir.path()), None);
    fs::write(
        dir.path().join("_link"),
        r#"<link project="server:database" package="mariadb">
  <patches><branch/></patches>
</link>"#,
    )
    .unwrap();
    assert_eq!(
        link_target(dir.path()),
        Some(("server:database".to_string(), "mariadb".to_string()))
    );
}

#[test]
fn a_flip_back_from_stale_does_not_restart_the_stall_timer() {
    assert!(resets_block_timer("blocked: waiting for rpm-build"));
    assert!(resets_block_timer("building"));
    assert!(!resets_block_timer("stale: outdated"));
}

#[test]
fn a_series_the_package_never_had_is_not_published() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("foo.spec"), "Name: foo\nPatch0: a.patch\n").unwrap();
    fs::write(dir.path().join("a.patch"), "x").unwrap();
    fs::write(dir.path().join("series"), "a.patch\n").unwrap();
    let plan = publish_plan(dir.path());
    assert!(plan.skipped.contains(&"series".to_string()), "{plan:?}");
    assert!(plan.files.contains(&"a.patch".to_string()));
    // One OBS tracks is kept.
    fs::create_dir_all(dir.path().join(".osc")).unwrap();
    fs::write(
        dir.path().join(".osc/_files"),
        r#"<directory><entry name="series" md5="x"/></directory>"#,
    )
    .unwrap();
    assert!(
        publish_plan(dir.path())
            .files
            .contains(&"series".to_string())
    );
}

#[test]
fn retry_stops_at_the_first_success() {
    let mut calls = 0;
    let r: Result<u32, &str> = retry(4, Duration::ZERO, || {
        calls += 1;
        if calls < 3 {
            Err("transient")
        } else {
            Ok(calls)
        }
    });
    assert_eq!(r, Ok(3));
}

#[test]
fn retry_gives_up_after_the_bound() {
    let mut calls = 0;
    let r: Result<(), &str> = retry(4, Duration::ZERO, || {
        calls += 1;
        Err("down")
    });
    assert_eq!(r, Err("down"));
    assert_eq!(calls, 4);
}
