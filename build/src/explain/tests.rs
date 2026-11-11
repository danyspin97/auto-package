use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const RUN: &str = "20261027-175210-aaaa-1-0";

fn events(extra: &[&str]) -> String {
    let mut v = vec![
            r#"{"event":"run_started","run_id":"R","argv":["auto-package","update","--project","p","--budget-usd","2.5","--work-dir","/w"],"work_dir":"/w"}"#.to_string(),
            r#"{"event":"target_selected","pkg":"foo","version":"2.0","reason":"FirstUnbuiltDependency","blocked_by":[],"from_version":"1.0"}"#.to_string(),
            r#"{"event":"target_selected","pkg":"other","version":"9","reason":"ExplicitOverride","blocked_by":[],"from_version":"9"}"#.to_string(),
            r#"{"pkg":"foo","event":"step_started","step":1,"kind":"AgentTurn","attempt":1,"round":0,"detail":"review"}"#.to_string(),
            r#"{"pkg":"foo","event":"agent_prompt","step":1,"task":{"FixAfterBuild":{"attempt":1,"max_attempts":10}}}"#.to_string(),
            r#"{"pkg":"foo","event":"agent_reply","step":1,"response":{"action":"Edited","reasoning":"Add a missing BuildRequires.","evidence":["Read build.log: nothing provides bar-devel"],"confidence":"High"},"usage":{"input_tokens":5,"output_tokens":100,"cost_usd":0.25}}"#.to_string(),
            r#"{"pkg":"foo","event":"spec_changed","step":1,"lines_added":3,"lines_removed":1}"#.to_string(),
            r#"{"pkg":"foo","event":"step_finished","step":1,"outcome":"Ok","elapsed":20555}"#.to_string(),
            r#"{"pkg":"foo","event":"warning","step":1,"message":"OBS build status still building after 132s with no change"}"#.to_string(),
            r#"{"pkg":"foo","event":"warning","step":1,"message":"OBS build status still building after 264s with no change"}"#.to_string(),
        ];
    v.extend(extra.iter().map(|s| s.to_string()));
    v.join("\n") + "\n"
}

fn fixture(extra: &[&str]) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(&d.join("runs").join(RUN).join("run.jsonl"), &events(extra));
    write(
        &d.join("runs")
            .join(RUN)
            .join("foo/002-agent-review/response.json"),
        "{}",
    );
    write(
        &d.join("state/foo/state.json"),
        r#"{"last_outcome":"failed","last_failure_reason":"gave up","obs_name":"foo","cumulative_spent_usd":0.25,"cumulative_spent_run_id":"20261027-175210-aaaa-1-0"}"#,
    );
    write(
        &d.join("proj/foo/foo.spec"),
        "Name: foo\nVersion:        2.0\n",
    );
    write(
        &d.join("proj/foo/foo.changes"),
        "-------------------------------------------------------------------\nMon Oct 26 10:00:00 UTC 2026 - a@b.c\n\n- Update to 2.0\n\n-------------------------------------------------------------------\nOld\n\n- Old entry\n",
    );
    t
}

fn query<'a>(d: &'a Path) -> Query<'a> {
    Query {
        project_dir: d,
        package: "foo",
        run: None,
        package_notes: String::new(),
    }
}

#[test]
fn a_failed_package_is_explained_from_its_files() {
    let t = fixture(&[
        r#"{"pkg":"foo","event":"step_started","step":2,"kind":"Build","attempt":1,"round":1,"detail":null}"#,
        r#"{"pkg":"foo","event":"build_failed","step":2,"classification":"CheckFailed","log":"LOG"}"#,
        r#"{"pkg":"foo","event":"step_finished","step":2,"outcome":"Failed","elapsed":61000}"#,
        r#"{"pkg":"foo","event":"package_finished","version":"2.0","outcome":{"Failed":{"reason":"gave up after 2 attempts"}}}"#,
        r#"{"event":"run_finished","outcome":"Failed","elapsed":70000}"#,
    ]);
    let d = t.path();
    write(
        &d.join("state/foo/build.log"),
        "[  5s] ok\n[ 10s] error: Bad exit status from /var/tmp/rpm-tmp (%check)\n[ 11s] + echo done\n",
    );
    let ex = explain(&query(d)).unwrap();
    assert_eq!(ex.run.as_deref(), Some(RUN));
    assert_eq!(ex.version.from.as_deref(), Some("1.0"));
    assert_eq!(ex.version.to.as_deref(), Some("2.0"));
    assert_eq!(ex.version.selections, 1);
    assert_eq!(ex.version.spec_version.as_deref(), Some("2.0"));
    assert_eq!(ex.outcome.result, "failed");
    assert_eq!(
        ex.outcome.detail.as_deref(),
        Some("gave up after 2 attempts")
    );
    assert_eq!(
        ex.outcome.log_errors,
        vec!["Bad exit status from /var/tmp/rpm-tmp (%check)"]
    );
    assert_eq!(ex.outcome.changelog.len(), 2);
    assert!(ex.outcome.changelog[1].contains("Update to 2.0"));
    assert_eq!(ex.attempts.len(), 2);
    let a = &ex.attempts[0];
    assert_eq!((a.spec_added, a.spec_removed), (Some(3), Some(1)));
    assert_eq!(a.agent_turn, Some(1));
    assert_eq!(
        a.agent.as_ref().unwrap().task.as_deref(),
        Some("FixAfterBuild 1/10")
    );
    assert!(a.dir.as_deref().unwrap().ends_with("002-agent-review"));
    assert_eq!(
        ex.attempts[1]
            .build
            .as_ref()
            .unwrap()
            .classification
            .as_deref(),
        Some("CheckFailed")
    );
    assert_eq!(ex.cost.total_usd, 0.25);
    assert_eq!(ex.cost.budget_usd, 2.5);
    assert!(!ex.cost.budget_is_default);
    assert_eq!(ex.cost.state_spent_usd, Some(0.25));
    // The other package's events and the two status polls are not mixed in.
    assert_eq!(ex.warnings.len(), 1);
    assert!(ex.warnings[0].message.contains("2 such status warnings"));
    assert_eq!(ex.run_info.finished.as_deref(), Some("Failed"));
}

#[test]
fn a_failed_turn_counts_as_a_turn_and_its_cost() {
    let t = fixture(&[
        r#"{"pkg":"foo","event":"failed_turn_usage","step":1,"usage":{"input_tokens":9,"output_tokens":40,"cost_usd":0.0532}}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert!(
        (ex.cost.total_usd - 0.3032).abs() < 1e-9,
        "{:?}",
        ex.cost.total_usd
    );
    assert_eq!(ex.cost.turns, 2);
}

#[test]
fn an_interrupted_run_says_so_and_invents_no_outcome() {
    let t = fixture(&[]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(ex.outcome.result, "no result recorded");
    assert!(ex.run_info.finished.is_none());
    let text = plain(&render(&ex, 80));
    assert!(text.contains("not finished"), "{text}");
    assert!(text.contains("interrupted or is still going"), "{text}");
    assert!(text.contains("model's own summary"), "{text}");
}

#[test]
fn the_latest_run_that_touched_the_package_is_the_default_and_run_picks_another() {
    let t = fixture(&[]);
    let d = t.path();
    let newer = "20261028-000000-bbbb-1-0";
    write(
        &d.join("runs").join(newer).join("run.jsonl"),
        "{\"event\":\"run_started\",\"argv\":[\"x\"]}\n",
    );
    let untouched = "20261029-000000-cccc-1-0";
    write(
        &d.join("runs").join(untouched).join("run.jsonl"),
        "{\"event\":\"run_started\",\"argv\":[\"x\"]}\n",
    );
    let ex = explain(&query(d)).unwrap();
    assert_eq!(
        ex.run.as_deref(),
        Some(RUN),
        "newer runs without the package are skipped"
    );
    let ex = explain(&Query {
        run: Some(newer),
        ..query(d)
    })
    .unwrap();
    assert_eq!(ex.run.as_deref(), Some(newer));
    assert_eq!(ex.other_runs, vec![RUN.to_string()]);
    assert!(
        explain(&Query {
            run: Some("nope"),
            ..query(d)
        })
        .is_err()
    );
}

#[test]
fn missing_files_and_garbage_lines_are_tolerated() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(
        &d.join("runs/r1/run.jsonl"),
        "not json {\"pkg\":\"foo\"\n{\"pkg\":\"foo\",\"event\":\"step_started\",\"step\":0,\"kind\":\"Checkout\"}\n",
    );
    let ex = explain(&query(d)).unwrap();
    assert_eq!(ex.run_info.unreadable_lines, 1);
    assert_eq!(ex.attempts.len(), 1);
    assert!(ex.unknown.iter().any(|u| u.contains("no state.json")));
    let nothing = tempfile::tempdir().unwrap();
    assert!(
        explain(&query(nothing.path()))
            .unwrap_err()
            .contains("nothing about foo")
    );
    assert!(
        explain(&Query {
            package: "../x",
            ..query(d)
        })
        .is_err()
    );
}

#[test]
fn flaky_verdicts_and_rejections_are_reported() {
    let t = fixture(&[
        r#"{"pkg":"foo","event":"spec_rejected","step":1,"reason":{"Placeholders":{"lines":["4: FIXME"]}},"restored_from":"/x"}"#,
        r#"{"pkg":"foo","event":"missing_packages","step":1,"names":["bar-devel"],"source":"BuildLog"}"#,
    ]);
    let d = t.path();
    write(
        &d.join("runs")
            .join(RUN)
            .join("foo/002-agent-review/prompt.md"),
        "x\n## Flaky tests (read by the pipeline)\n\nintro\n- Failed in ONE build only (flaky; convention 7 (section 87)): t1\n- Failed in BOTH builds (a real failure): t2\nDo not fix\n",
    );
    let ex = explain(&Query {
        package_notes: "- flaky tests (failed in one of two builds): t1; stable failures: t2\n"
            .into(),
        ..query(d)
    })
    .unwrap();
    assert_eq!(ex.flaky.len(), 3, "{:?}", ex.flaky);
    assert!(
        ex.warnings
            .iter()
            .any(|w| w.message.contains("rejected") && w.message.contains("FIXME"))
    );
    assert_eq!(
        ex.attempts[0].build.as_ref().unwrap().missing,
        vec!["bar-devel"]
    );
    assert_eq!(
        ex.attempts[0].rejected.as_deref(),
        Some("Placeholders: 4: FIXME")
    );
}

#[test]
fn the_text_wraps_to_the_width_and_colours_only_when_asked() {
    let t = fixture(&[]);
    let notes = "# foo\n".to_string() + &"word ".repeat(80);
    let ex = explain(&Query {
        package_notes: notes,
        ..query(t.path())
    })
    .unwrap();
    for width in [40, 72] {
        let lines = render(&ex, width);
        for l in &lines {
            let n: usize = l.iter().map(|s| s.text.chars().count()).sum();
            assert!(n <= width, "{n} > {width}: {l:?}");
        }
    }
    let lines = render(&ex, 80);
    assert!(!ansi(&lines, false).contains('\u{1b}'));
    assert!(ansi(&lines, true).contains("\u{1b}[1m"));
    let json = serde_json::to_value(&ex).unwrap();
    assert_eq!(json["package"], "foo");
    assert_eq!(json["attempts"][0]["agent"]["action"], "Edited");
}

#[test]
fn the_project_directory_is_resolved_like_the_other_commands() {
    let t = tempfile::tempdir().unwrap();
    let w = t.path();
    write(&w.join("home_x_p-x86_64/state/foo/state.json"), "{}");
    write(&w.join("home_y_q-x86_64/state/bar/state.json"), "{}");
    let one = resolve_project_dir(w, None, "x86_64", "foo").unwrap();
    assert!(one.ends_with("home_x_p-x86_64"));
    assert!(resolve_project_dir(w, None, "x86_64", "nope").is_err());
    let by_name = resolve_project_dir(w, Some("home:x:p"), "x86_64", "foo").unwrap();
    assert!(by_name.ends_with("home_x_p-x86_64"));
    assert_eq!(
        resolve_project_dir(&one, None, "x86_64", "foo").unwrap(),
        one
    );
    write(&w.join("home_y_q-x86_64/state/foo/state.json"), "{}");
    assert!(
        resolve_project_dir(w, None, "x86_64", "foo")
            .unwrap_err()
            .contains("several")
    );
}

#[test]
fn a_source_project_finds_its_one_branch_directory() {
    let t = tempfile::tempdir().unwrap();
    let w = t.path();
    write(
        &w.join("home_alex_branches_Base_System-x86_64/state/foo/state.json"),
        "{}",
    );
    write(
        &w.join("home_alex_branches_Other-x86_64/state/foo/state.json"),
        "{}",
    );
    let found = resolve_project_dir(w, Some("Base:System"), "x86_64", "foo").unwrap();
    assert!(found.ends_with("home_alex_branches_Base_System-x86_64"));
    write(
        &w.join("home_bob_branches_Base_System-x86_64/state/foo/state.json"),
        "{}",
    );
    let err = resolve_project_dir(w, Some("Base:System"), "x86_64", "foo").unwrap_err();
    assert!(err.contains("--branch-project"), "{err}");
    let err = resolve_project_dir(w, Some("Nope"), "x86_64", "foo").unwrap_err();
    assert!(err.contains("no work directory for Nope"), "{err}");
}

/// A run records the build that wrote it, and a log from before the field
/// has no build to show.
#[test]
fn explain_names_the_build_that_wrote_the_run() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(
        &d.join("runs").join(RUN).join("run.jsonl"),
        "{\"event\":\"run_started\",\"run_id\":\"R\",\"argv\":[\"auto-package\",\"update\"],\"work_dir\":\"/w\",\"build\":\"3f2a9c1d8e7b\"}\n\
         {\"event\":\"target_selected\",\"pkg\":\"foo\",\"version\":\"2.0\",\"reason\":\"RequestedRoot\",\"blocked_by\":[],\"from_version\":\"1.0\"}\n",
    );
    let ex = explain(&query(d)).unwrap();
    assert_eq!(ex.run_info.build.as_deref(), Some("3f2a9c1d8e7b"));
    let text = plain(&render(&ex, 80));
    assert!(
        text.lines()
            .any(|l| l.trim_start().starts_with("build") && l.contains("3f2a9c1d8e7b")),
        "{text}"
    );

    let old_log = fixture(&[]);
    let ex = explain(&query(old_log.path())).unwrap();
    assert_eq!(ex.run_info.build, None);
}

#[test]
fn the_hosts_a_run_allowed_are_listed_and_old_logs_list_none() {
    let t = fixture(&[
        r#"{"event":"host_allowed","host":"a.example.org","scope":"run","source":"flag"}"#,
        r#"{"event":"host_allowed","host":"b.example.org","scope":"persistent","source":"tui"}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(
        ex.run_info.hosts_allowed,
        [
            "a.example.org (run, from flag)",
            "b.example.org (persistent, from tui)"
        ]
    );
    let text = plain(&render(&ex, 100));
    assert!(text.contains("hosts allowed"), "{text}");
    assert!(
        text.contains("b.example.org (persistent, from tui)"),
        "{text}"
    );
    // A log from before the event existed reads as before: no line.
    let old = fixture(&[]);
    let ex = explain(&query(old.path())).unwrap();
    assert!(ex.run_info.hosts_allowed.is_empty());
    assert!(!plain(&render(&ex, 100)).contains("hosts allowed"));
}

#[test]
fn the_other_arch_results_that_are_not_green_are_listed() {
    let t = fixture(&[
        r#"{"event":"arch_check","package":"foo","results":[{"repo":"ports","arch":"s390x","state":"failed","detail":"boom","origin_fails":false},{"repo":"ports","arch":"aarch64","state":"succeeded","origin_fails":false}]}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(ex.run_info.arch_checks, ["ports/s390x: failed (boom)"]);
    assert!(plain(&render(&ex, 100)).contains("other arches"));
    let old = fixture(&[]);
    let ex = explain(&query(old.path())).unwrap();
    assert!(ex.run_info.arch_checks.is_empty());
}

#[test]
fn a_port_result_that_fails_in_factory_too_says_so() {
    let t = fixture(&[
        r#"{"event":"arch_check","package":"foo","results":[{"repo":"openSUSE_Factory_zSystems","arch":"s390x","state":"not testable","detail":"nothing provides libwlroots-0_20","origin_fails":false},{"repo":"openSUSE_Factory_PowerPC","arch":"ppc64le","state":"failed","origin_fails":true},{"repo":"ports","arch":"aarch64","state":"failed","origin_fails":true}]}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(
        ex.run_info.arch_checks,
        [
            "openSUSE_Factory_zSystems/s390x: not testable (nothing provides libwlroots-0_20)",
            "openSUSE_Factory_PowerPC/ppc64le: failed [also fails in Factory]",
            "ports/aarch64: failed [also fails in the origin]",
        ]
    );
}

#[test]
fn a_built_package_that_fails_on_another_arch_says_so_in_its_result() {
    let t = fixture(&[
        r#"{"pkg":"foo","event":"package_finished","version":"2.0","outcome":"Built"}"#,
        r#"{"event":"arch_check","package":"foo","results":[{"repo":"openSUSE_Factory","arch":"riscv64","state":"failed","origin_fails":false,"origin_missing":true},{"repo":"openSUSE_Factory","arch":"aarch64","state":"succeeded","origin_fails":false}]}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(
        ex.outcome.result,
        "built (fails on openSUSE_Factory/riscv64)"
    );
    let ok = fixture(&[
        r#"{"pkg":"foo","event":"package_finished","version":"2.0","outcome":"Built"}"#,
        r#"{"event":"arch_check","package":"foo","results":[{"repo":"openSUSE_Factory","arch":"aarch64","state":"succeeded","origin_fails":false}]}"#,
    ]);
    let ex = explain(&query(ok.path())).unwrap();
    assert_eq!(ex.outcome.result, "built");
}

#[test]
fn a_failure_the_origin_has_no_result_for_says_it_is_not_blocking() {
    let t = fixture(&[
        r#"{"event":"arch_check","package":"foo","results":[{"repo":"openSUSE_Factory","arch":"riscv64","state":"failed","origin_fails":false,"origin_missing":true}]}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(
        ex.run_info.arch_checks,
        ["openSUSE_Factory/riscv64: failed [no result in the origin: not blocking]"]
    );
}

#[test]
fn the_zig_vendor_archive_a_run_refreshed_lists_its_dependencies() {
    let t = fixture(&[
        r#"{"event":"zig_vendor","step":2,"pkg":"foo","file":"vendor.tar.zst","changed":true,"deps":[{"name":"pixman","url":"https://codeberg.org/ifreund/zig-pixman/archive/v0.3.0.tar.gz","hash":"pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX"}]}"#,
    ]);
    let ex = explain(&query(t.path())).unwrap();
    assert_eq!(ex.run_info.zig_vendor.len(), 2);
    assert_eq!(
        ex.run_info.zig_vendor[0],
        "vendor.tar.zst: rewritten, 1 dependency"
    );
    assert!(ex.run_info.zig_vendor[1].contains("pixman-0.3.0-LClMn"));
    let text = plain(&render(&ex, 120));
    assert!(text.contains("zig vendor"), "{text}");
    assert!(text.contains("vendor.tar.zst: rewritten"), "{text}");
    // A run without the event lists none.
    let old = fixture(&[]);
    let ex = explain(&query(old.path())).unwrap();
    assert!(ex.run_info.zig_vendor.is_empty());
}

/// `text` without its SGR escapes (`ESC [ ... m`).
fn strip_escapes(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn each_kind_is_painted_in_its_role_and_plain_is_never_styled() {
    use ap_core::sink::{Role, paint};
    let cases = [
        (Kind::Heading, Some(Role::Heading)),
        (Kind::Label, Some(Role::Info)),
        (Kind::Good, Some(Role::Success)),
        (Kind::Bad, Some(Role::Failure)),
        (Kind::Warn, Some(Role::Warning)),
        (Kind::Dim, Some(Role::Dim)),
        (Kind::Plain, None),
    ];
    for (kind, role) in cases {
        let line = vec![span("text", kind)];
        assert_eq!(ansi(std::slice::from_ref(&line), false), "text\n");
        let on = ansi(std::slice::from_ref(&line), true);
        match role {
            Some(r) => assert_eq!(on, format!("{}\n", paint("text", r, true)), "{kind:?}"),
            None => assert_eq!(on, "text\n", "{kind:?}"),
        }
    }
}

#[test]
fn stripping_the_escapes_gives_the_plain_report() {
    let t = fixture(&[]);
    let ex = explain(&query(t.path())).unwrap();
    let lines = render(&ex, 80);
    let plain_text = ansi(&lines, false);
    let coloured = ansi(&lines, true);
    assert!(coloured.contains('\u{1b}'));
    assert_eq!(strip_escapes(&coloured), plain_text);
    assert_eq!(plain(&lines) + "\n", plain_text);
}

#[test]
fn json_stays_free_of_escapes_even_when_colour_is_on() {
    let t = fixture(&[]);
    let ex = explain(&query(t.path())).unwrap();
    assert!(ansi(&render(&ex, 80), true).contains('\u{1b}'));
    let json = serde_json::to_string_pretty(&ex).unwrap();
    assert!(!json.contains('\u{1b}'));
}
