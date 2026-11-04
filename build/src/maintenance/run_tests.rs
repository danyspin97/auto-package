use super::*;

use super::helpers::*;

#[test]
pub(super) fn the_request_has_one_incident_action_per_code_stream() {
    let x = request_xml(&request());
    assert_eq!(
        x.matches("<action type=\"maintenance_incident\">").count(),
        2
    );
    assert!(x.contains(
            "    <source project=\"home:u:branches:SUSE:SLE-15-SP7:Update\" package=\"curl\"/>\n    \
             <target project=\"SUSE:Maintenance\" releaseproject=\"SUSE:SLE-15-SP7:Update\"/>\n    \
             <options>\n      <sourceupdate>cleanup</sourceupdate>\n    </options>\n"
        ), "{x}");
    assert!(
        x.contains("<description>- Update to 8.1 (bsc#1234567) &amp; more &lt;x&gt;</description>")
    );
    let mut kept = request();
    kept.sourceupdate = None;
    assert!(!request_xml(&kept).contains("<options>"));
    assert_eq!(CREATE_REQUEST_PATH, "/request?cmd=create&addrevision=1");
}

#[test]
pub(super) fn open_incidents_are_found_per_source_and_release_project() {
    assert_eq!(
        open_incidents_path("home:u:branches:X:Update", "curl"),
        "/request?view=collection&roles=source&types=maintenance_incident&states=new,review,accepted&project=home%3Au%3Abranches%3AX%3AUpdate&package=curl"
    );
    let xml = r#"<collection matches="2"><request id="1382124" creator="u">
  <action type="maintenance_incident">
    <source project="home:u:branches:X:Update" package="curl" rev="c9"/>
    <target project="openSUSE:Maintenance" releaseproject="X:Update"/>
  </action>
  <state name="new" who="bot" when="2026-10-02T14:10:21"/>
</request>
<request id="7">
  <action type="submit">
    <source project="home:u:branches:X:Update" package="curl"/>
    <target project="X" package="curl"/>
  </action>
  <state name="review"/>
</request></collection>"#;
    let open = parse_open_incidents(xml);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, "1382124");
    assert_eq!(open[0].state, "new");
    assert_eq!(open[0].release_project.as_deref(), Some("X:Update"));
    assert_eq!(open[0].rev.as_deref(), Some("c9"));
    assert!(
        find_open_incident(&open, "home:u:branches:X:Update", "curl", "X:Update", &[]).is_some()
    );
    assert!(
        find_open_incident(&open, "home:u:branches:X:Update", "curl", "Y:Update", &[]).is_none()
    );
    assert!(
        find_open_incident(&open, "home:u:branches:X:Update", "wget", "X:Update", &[]).is_none()
    );
}

#[test]
pub(super) fn the_description_is_the_changelog_entry_with_its_references() {
    let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
    let entry = "- Update to 1.2.3 (bsc#1234567, CVE-2025-1234):\n  * fix".to_string();
    let same = [
        ("A".to_string(), Some(entry.clone())),
        ("B".to_string(), Some(entry.clone())),
    ];
    let d = request_description("curl", "", &same, &refs);
    assert!(d.starts_with(&entry), "{d}");
    assert!(!d.contains("References:"), "{d}");
    let differ = [
        ("A".to_string(), Some(entry.clone())),
        ("B".to_string(), Some("- Other".into())),
    ];
    let d = request_description("curl", "", &differ, &refs);
    assert!(
        d.starts_with("A:\n- Update to 1.2.3") && d.contains("\n\nB:\n- Other"),
        "{d}"
    );
    let d = request_description("curl", "", &[("A".into(), None)], &refs);
    assert!(
        d.starts_with("Maintenance update of curl.\n\nReferences: bsc#1234567, CVE-2025-1234"),
        "{d}"
    );
}

#[test]
pub(super) fn the_plan_names_every_code_stream_the_references_and_what_is_left_out() {
    let lines = plan_lines(
        &request(),
        &["bsc#1234567".into()],
        "",
        &[],
        &[(
            "SUSE:SLE-12-SP5:Update".into(),
            "request 9 is already open".into(),
        )],
    );
    assert_eq!(
        lines[0],
        "maintenance request to SUSE:Maintenance (2 code streams):"
    );
    assert!(
        lines.contains(
            &"  home:u:branches:SUSE:SLE-15-SP7:Update/curl -> release in SUSE:SLE-15-SP7:Update"
                .to_string()
        )
    );
    assert!(
        lines
            .contains(&"  SUSE:SLE-12-SP5:Update: left out, request 9 is already open".to_string())
    );
    assert!(lines.contains(&"  bug references: bsc#1234567".to_string()));
    assert!(lines.contains(&"  on accept: the branch package is removed".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("    | - Update to 8.1")));
    let none = plan_lines(&request(), &[], "", &[], &[]);
    assert!(none.contains(&"  bug references: none (--no-bug-ref)".to_string()));
}

#[test]
pub(super) fn the_mode_defaults_to_backport_and_lint_takes_none() {
    assert_eq!(Mode::parse(None, false).unwrap(), Mode::Backport);
    assert_eq!(Mode::parse(Some("UPDATE"), false).unwrap(), Mode::Update);
    assert_eq!(
        Mode::parse(Some("backport"), false).unwrap(),
        Mode::Backport
    );
    assert_eq!(Mode::parse(None, true).unwrap(), Mode::Lint);
    assert!(Mode::parse(Some("update"), true).is_err());
    assert!(
        Mode::parse(Some("bump"), false)
            .unwrap_err()
            .contains("bump")
    );
}

#[test]
pub(super) fn backport_flags_must_fit_the_mode() {
    let one = vec!["curl".to_string()];
    let fix = vec!["fix.patch".to_string()];
    assert!(validate_mode(Mode::Backport, &fix, None, false, &one).is_ok());
    assert!(
        validate_mode(Mode::Backport, &[], None, false, &one)
            .unwrap_err()
            .contains("--backport")
    );
    assert!(
        validate_mode(Mode::Backport, &fix, None, true, &one)
            .unwrap_err()
            .contains("--no-bug-ref")
    );
    let two = vec!["curl".to_string(), "wget".to_string()];
    assert!(
        validate_mode(Mode::Backport, &fix, None, false, &two)
            .unwrap_err()
            .contains("one fix of one package")
    );
    assert!(validate_mode(Mode::Update, &fix, None, false, &two).is_err());
    assert!(validate_mode(Mode::Lint, &[], Some("https://github.com/o/r"), false, &one).is_err());
    assert!(validate_mode(Mode::Update, &[], None, true, &two).is_ok());
    assert!(
        validate_mode(
            Mode::Backport,
            &fix,
            Some("https://github.com/o/r"),
            false,
            &one
        )
        .is_ok()
    );
}

#[test]
pub(super) fn the_plan_and_the_description_say_which_mode() {
    let j = job();
    let backport = mode_line(Mode::Backport, Some(&j));
    assert_eq!(
        backport,
        "Backport of CVE-2025-1234.patch (https://github.com/o/r/commit/abc1234.patch) to the \
             version each code stream has (no version update)."
    );
    let update = mode_line(Mode::Update, None);
    assert!(update.starts_with("Version update"), "{update}");
    assert!(mode_line(Mode::Lint, None).contains("keeps its version"));
    let d = request_description("curl", &backport, &[("A".into(), Some("- x".into()))], &[]);
    assert!(d.starts_with("Backport of CVE-2025-1234.patch"), "{d}");
    assert!(d.contains("\n\n- x\n\n"), "{d}");
    let lines = plan_lines(
        &request(),
        &j.refs,
        &backport,
        &[(
            "SUSE:SLE-15-SP7:Update".into(),
            "8.6.0: CVE-2025-1234.patch applied after a refresh for this code stream".into(),
        )],
        &[],
    );
    assert_eq!(lines[1], format!("  mode: {backport}"));
    assert!(lines.contains(&"  home:u:branches:SUSE:SLE-15-SP7:Update/curl -> release in SUSE:SLE-15-SP7:Update: 8.6.0: CVE-2025-1234.patch applied after a refresh for this code stream".to_string()), "{lines:?}");
    assert!(
        lines.contains(
            &"  home:u:branches:SUSE:SLE-15-SP6:Update/curl -> release in SUSE:SLE-15-SP6:Update"
                .to_string()
        ),
        "{lines:?}"
    );
}

#[test]
pub(super) fn a_backport_keeps_the_version_of_the_code_stream() {
    assert_eq!(version_kept(Some("8.6.0"), Some("8.6.0")).unwrap(), "8.6.0");
    assert!(
        version_kept(Some("8.6.0"), Some("8.11.0"))
            .unwrap_err()
            .contains("from 8.6.0 to 8.11.0")
    );
    assert!(version_kept(None, Some("1")).is_err());
    assert!(version_kept(Some("1"), None).is_err());
}

#[test]
pub(super) fn the_changelog_gets_the_backport_bullet_and_the_missing_references() {
    let j = job();
    let bullets = backport_bullets(Some(&j), &j.refs);
    assert_eq!(
        bullets,
        vec![(
            "CVE-2025-1234.patch".to_string(),
            "- Add CVE-2025-1234.patch: reject overlong header lines (bsc#1234567, CVE-2025-1234)"
                .to_string()
        )]
    );
    let changes = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Add CVE-2025-1234.patch to fix the parser

-------------------------------------------------------------------
Mon Sep  1 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Update to 8.6.0
";
    let got = amend_changes(changes, &j.refs, &bullets).unwrap();
    assert!(
        got.contains(&format!("<dev@suse.com>\n\n{}\n", bullets[0].1)),
        "{got}"
    );
    assert_eq!(got.matches("bsc#1234567").count(), 1, "{got}");
    assert_eq!(amend_changes(&got, &j.refs, &bullets), None);
    // An update run only adds the references.
    let got = amend_changes(changes, &["bsc#1234567".to_string()], &[]).unwrap();
    assert!(got.contains("fix the parser (bsc#1234567)"), "{got}");
    assert!(backport_bullets(None, &j.refs).is_empty());
}

#[test]
pub(super) fn the_stream_check_seam_filters_only_what_it_rules_out() {
    let streams = vec![
        "A:Update".to_string(),
        "B:Update".to_string(),
        "C:Update".to_string(),
    ];
    let refs = vec!["CVE-2025-1234".to_string()];
    let fix = FixQuery {
        refs: &refs,
        sources: &[],
    };
    let none = verdicts(&NoStreamCheck, "curl", &streams, &fix);
    assert!(none.iter().all(|(_, a)| *a == Affected::Unknown));
    assert_eq!(streams_to_build(&streams, &[none]), streams);
    // A check that answers for fewer streams leaves the rest unknown.
    let some = verdicts(
        &Fixed(vec![
            Affected::Affected,
            Affected::NotAffected("the code is not in 1.0".into()),
        ]),
        "curl",
        &streams,
        &fix,
    );
    assert_eq!(some[2].1, Affected::Unknown);
    assert_eq!(
        streams_to_build(&streams, std::slice::from_ref(&some)),
        vec!["A:Update".to_string(), "C:Update".to_string()]
    );
    // With several packages a stream is left out only when none needs it.
    let all = vec![(streams[1].clone(), Affected::Unknown)];
    assert_eq!(streams_to_build(&streams, &[some, all]), streams);
}

#[test]
pub(super) fn the_affected_by_command_gets_package_stream_and_fix_and_its_answer_is_parsed() {
    let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
    let sources = vec!["https://x/y.patch".to_string()];
    let fix = FixQuery {
        refs: &refs,
        sources: &sources,
    };
    assert_eq!(
        affected_by_args("curl", "SUSE:SLE-15-SP7:Update", &fix),
        vec![
            "curl",
            "SUSE:SLE-15-SP7:Update",
            "bsc#1234567,CVE-2025-1234",
            "https://x/y.patch"
        ]
    );
    assert_eq!(parse_affected("affected\n", true), Affected::Affected);
    assert_eq!(
        parse_affected("\nnot-affected  the code came in 8.10\n", true),
        Affected::NotAffected("the code came in 8.10".into())
    );
    assert!(
        matches!(parse_affected("not-affected", true), Affected::NotAffected(w) if !w.is_empty())
    );
    assert_eq!(parse_affected("unknown", true), Affected::Unknown);
    assert_eq!(parse_affected("affected", false), Affected::Unknown);
    assert_eq!(parse_affected("yes", true), Affected::Unknown);
    assert_eq!(parse_affected("affected maybe", true), Affected::Unknown);
    assert_eq!(parse_affected("", true), Affected::Unknown);
    assert_eq!(
        parse_affected("not-affected a\u{1b}[31mb", true),
        Affected::NotAffected("a[31mb".into())
    );
}

#[test]
pub(super) fn an_affected_by_command_that_fails_or_cannot_start_is_unknown() {
    let refs = vec!["CVE-2025-1234".to_string()];
    let fix = FixQuery {
        refs: &refs,
        sources: &[],
    };
    let missing = CommandCheck {
        program: "/nonexistent/affected-by-check".into(),
        timeout: Duration::from_secs(5),
    };
    assert_eq!(
        missing.check("curl", &["A:Update".to_string()], &fix),
        vec![Affected::Unknown]
    );
}

#[test]
pub(super) fn every_code_stream_ends_in_the_summary_and_the_plan() {
    let reports = vec![
        StreamReport {
            stream: "A:Update".into(),
            outcome: StreamOutcome::Ready("8.6.0: CVE-2025-1234.patch applied cleanly".into()),
        },
        StreamReport {
            stream: "B:Update".into(),
            outcome: StreamOutcome::Failed(
                "the backport did not go in: the branch has no CVE-2025-1234.patch".into(),
            ),
        },
        StreamReport {
            stream: "C:Update".into(),
            outcome: StreamOutcome::NotAffected("the code came in 8.10".into()),
        },
        StreamReport {
            stream: "D:Update".into(),
            outcome: StreamOutcome::LeftOut(
                "request 9 from home:u:branches:D:Update is already open".into(),
            ),
        },
        StreamReport {
            stream: "[git] pool/curl:leap-16.0".into(),
            outcome: StreamOutcome::GitMaintained,
        },
    ];
    let s = summary_lines("curl", &reports);
    assert_eq!(
        s[0],
        "curl: 1 of 5 code streams ready for the maintenance request:"
    );
    assert_eq!(
        s[1],
        "  A:Update: ready, 8.6.0: CVE-2025-1234.patch applied cleanly"
    );
    assert!(
        s[2].starts_with("  B:Update: FAILED, the backport did not go in"),
        "{s:?}"
    );
    assert_eq!(s[3], "  C:Update: not affected, the code came in 8.10");
    assert!(s[4].starts_with("  D:Update: left out, request 9"), "{s:?}");
    assert_eq!(
        s[5],
        "  [git] pool/curl:leap-16.0: skipped: maintained in git (SLFO), which is not supported yet"
    );
    let out = left_out(&reports);
    assert_eq!(out.len(), 4);
    assert!(out.iter().all(|(s, _)| s != "A:Update"));
    assert_eq!(
        out[0].1,
        "failed: the backport did not go in: the branch has no CVE-2025-1234.patch"
    );
    assert_eq!(out[3].1, "maintained in git (SLFO), not supported yet");
}

#[test]
pub(super) fn git_maintained_streams_are_read_from_osc_maintained() {
    let text = "openSUSE:Backports:SLE-15-SP7:Update/curl\n\
                    [git] pool/curl:leap-16.0\n\
                    [git] products/curl:slfo-main\n\
                    [git] broken\n";
    let got = parse_git_maintained(text);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].to_string(), "[git] pool/curl:leap-16.0");
    assert_eq!(got[1].branch, "slfo-main");
    assert!(parse_git_maintained("A:Update/curl\n").is_empty());
}

#[test]
pub(super) fn an_accepted_request_counts_only_at_the_current_source_revision() {
    // The shape OBS returns: the accepted request's target is the incident
    // and `rev` is the branch link's expanded srcmd5 (request 1249558).
    let xml = r#"<collection matches="2">
<request id="11" creator="u">
  <action type="maintenance_incident">
    <source project="home:u:branches:X:Update" package="curl" rev="30e3f10b756fafa3289949583302ec27"/>
    <target project="openSUSE:Maintenance:19468" releaseproject="X:Update"/>
  </action>
  <state name="accepted" who="m" when="2026-10-02T14:10:21"/>
</request>
<request id="12" creator="u">
  <action type="maintenance_incident">
    <source project="home:u:branches:X:Update" package="curl" rev="aaaa"/>
    <target project="openSUSE:Maintenance:19000" releaseproject="X:Update"/>
  </action>
  <state name="accepted" who="m"/>
</request></collection>"#;
    let found = parse_open_incidents(xml);
    assert_eq!(found.len(), 2);
    let dir = r#"<directory name="curl" rev="3" vrev="3" srcmd5="406bf488d1c3eb9bf362555e09055d5b">
  <linkinfo project="X:Update" package="curl" srcmd5="52c1" baserev="52c1" xsrcmd5="30e3f10b756fafa3289949583302ec27" lsrcmd5="406b"/>
  <entry name="_link" md5="8b" size="148" mtime="1"/>
</directory>"#;
    let current = parse_source_revisions(dir);
    assert_eq!(
        current,
        vec![
            "3".to_string(),
            "406bf488d1c3eb9bf362555e09055d5b".to_string(),
            "30e3f10b756fafa3289949583302ec27".to_string()
        ]
    );
    let hit = find_open_incident(
        &found,
        "home:u:branches:X:Update",
        "curl",
        "X:Update",
        &current,
    )
    .unwrap();
    assert_eq!(hit.id, "11");
    assert_eq!(
        submitted_reason(hit, "home:u:branches:X:Update"),
        "request 11 already submitted this revision of home:u:branches:X:Update (accepted)"
    );
    // A newer branch revision is a new change: the accepted ones do not count.
    assert!(
        find_open_incident(
            &found,
            "home:u:branches:X:Update",
            "curl",
            "X:Update",
            &["bbbb".into()]
        )
        .is_none()
    );
    // An accepted request without a pinned revision proves nothing.
    let mut unpinned = found.clone();
    unpinned.iter_mut().for_each(|o| o.rev = None);
    assert!(
        find_open_incident(
            &unpinned,
            "home:u:branches:X:Update",
            "curl",
            "X:Update",
            &current
        )
        .is_none()
    );
    let open = OpenIncident {
        state: "review".into(),
        ..found[1].clone()
    };
    assert_eq!(
        submitted_reason(&open, "B"),
        "request 12 from B is already open"
    );
    assert!(parse_source_revisions("<nope/>").is_empty());
}

#[test]
pub(super) fn a_maintenance_run_refuses_build_paths_with_a_clear_reason() {
    assert_eq!(build_path_refusal("curl", &[]), None);
    let why = build_path_refusal("curl", &["devel:libraries:c_c++/SLE_15_SP7".into()]).unwrap();
    assert!(
        why.starts_with("curl needs the build path devel:libraries:c_c++/SLE_15_SP7"),
        "{why}"
    );
    assert!(why.contains("not added"), "{why}");
}

#[test]
pub(super) fn a_running_incident_is_named_as_one() {
    let w = devel_warning(
        "openSUSE:Backports:SLE-15-SP7:Update",
        "tor",
        &(
            "openSUSE:Maintenance:19709".into(),
            "tor.openSUSE_Backports_SLE-15-SP7_Update".into(),
        ),
    );
    assert!(w.contains("an incident"), "{w}");
    let w = devel_warning("L:Update", "curl", &("S:Update".into(), "curl".into()));
    assert!(w.contains("a fix normally goes to that code stream"), "{w}");
}

#[test]
fn a_run_of_only_yes_refusals_sends_nothing_and_is_refused() {
    let refusal = |stream: &str| StreamReport {
        stream: stream.into(),
        outcome: StreamOutcome::Failed(
            "--yes refuses to submit a change that needs a review".into(),
        ),
    };
    let failures = vec!["python-foo in SLE: --yes refuses".to_string()];
    assert_eq!(
        end_of_run(&[vec![refusal("SLE")]], failures.clone(), 1),
        Err(RunError::Refused(failures[0].clone()))
    );
    // A stream that went into a request means something was sent: a failure.
    let ready = StreamReport {
        stream: "SLE-2".into(),
        outcome: StreamOutcome::Ready("applied cleanly".into()),
    };
    assert_eq!(
        end_of_run(&[vec![refusal("SLE"), ready]], failures.clone(), 1),
        Err(RunError::Failed(failures[0].clone()))
    );
    assert_eq!(end_of_run(&[], Vec::new(), 0), Ok(()));
}
