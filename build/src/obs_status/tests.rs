use super::*;

const REAL: &str = r#"<resultlist state="198a2ea6aaa8c09f5cc9e4de90d1fa74">
  <result project="home:u:p" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="oatpp" code="failed"/>
  </result>
  <result project="home:u:p" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building" dirty="true">
    <status package="oatpp" code="finished">
      <details>failed</details>
    </status>
    <status package="other" code="blocked"><details>waiting for 1 compiles</details></status>
  </result>
</resultlist>"#;

fn entry(code: &str, details: Option<&str>, dirty: bool) -> ResultEntry {
    ResultEntry {
        repo: "r".into(),
        arch: "a".into(),
        dirty,
        pkg: "p".into(),
        code: code.into(),
        details: details.map(str::to_string),
    }
}

fn evidence(src: &str, job_src: &str, job_code: &str) -> Evidence {
    Evidence {
        source: Some(SourceInfo {
            srcmd5: src.into(),
            verifymd5: None,
        }),
        job: Some(JobInfo {
            srcmd5: job_src.into(),
            verifymd5: None,
            code: job_code.into(),
        }),
        confirm: false,
    }
}

#[test]
fn parses_real_result_document() {
    let all = parse_results(REAL);
    assert_eq!(all.len(), 3);
    let x86 = find_entry(REAL, "openSUSE_Tumbleweed", "x86_64", "oatpp").unwrap();
    assert!(x86.dirty);
    assert_eq!(x86.code, "finished");
    assert_eq!(x86.details.as_deref(), Some("failed"));
    let i586 = find_entry(REAL, "openSUSE_Tumbleweed", "i586", "oatpp").unwrap();
    assert!(!i586.dirty);
    assert_eq!(i586.details, None);
    let other = find_entry(REAL, "openSUSE_Tumbleweed", "x86_64", "other").unwrap();
    assert_eq!(other.details.as_deref(), Some("waiting for 1 compiles"));
    assert!(find_entry(REAL, "openSUSE_Tumbleweed", "x86_64", "nope").is_none());
}

#[test]
fn a_cut_off_document_is_an_error() {
    let cut = &REAL[..REAL.find("package=\"other\"").unwrap() + 5];
    assert!(try_parse_results(cut).is_err());
    // Cut between tags, inside an open <result>.
    let cut = &REAL[..REAL.find("<status package=\"other\"").unwrap()];
    assert!(try_parse_results(cut).is_err());
    assert_eq!(try_parse_results(REAL).unwrap().len(), 3);
    assert!(try_parse_results("").unwrap().is_empty());
}

#[test]
fn attribute_order_quotes_and_entities_do_not_matter() {
    let xml = "<?xml version='1.0'?><resultlist><result arch='x86_64' dirty='true' repository='r'>\
                   <status code='succeeded' package='a&amp;b'/></result></resultlist>";
    let e = &parse_results(xml)[0];
    assert_eq!(
        (e.pkg.as_str(), e.code.as_str(), e.dirty),
        ("a&b", "succeeded", true)
    );
}

#[test]
fn numeric_references_in_attributes_are_decoded() {
    let xml = "<resultlist><result repository='r' arch='a'><status package='a&#43;&#43;' code='succeeded'/></result></resultlist>";
    assert_eq!(parse_results(xml)[0].pkg, "a++");
}

#[test]
fn garbage_and_truncation_yield_nothing_not_a_panic() {
    assert!(parse_results("").is_empty());
    assert!(parse_results("not xml at all").is_empty());
    assert!(
        parse_results("<resultlist><result repository=\"r\" arch=\"a\"><status pack").is_empty()
    );
    assert!(parse_source_info("<sourceinfo srcmd5=").is_none());
    assert!(parse_latest_job("<jobhistlist><jobhist srcmd5=\"").is_none());
}

#[test]
fn active_states_map_directly() {
    for (code, want) in [
        ("scheduled", BuildStatus::Scheduled),
        ("building", BuildStatus::Building),
        ("dispatching", BuildStatus::Building),
        ("signing", BuildStatus::Building),
        ("blocked", BuildStatus::Blocked),
    ] {
        assert_eq!(
            resolve(&entry(code, None, true), &Evidence::default()),
            want
        );
    }
    assert_eq!(
        resolve(&entry("locked", None, false), &Evidence::default()),
        BuildStatus::Unknown("locked".into())
    );
}

#[test]
fn clean_repo_trusts_the_verdict_including_finished_details() {
    let none = Evidence::default();
    assert_eq!(
        resolve(&entry("succeeded", None, false), &none),
        BuildStatus::Succeeded
    );
    assert_eq!(
        resolve(&entry("finished", Some("succeeded"), false), &none),
        BuildStatus::Succeeded
    );
    assert_eq!(
        resolve(&entry("finished", Some("failed"), false), &none),
        BuildStatus::Failed
    );
}

#[test]
fn bare_finished_without_details_is_not_terminal() {
    let s = resolve(&entry("finished", None, false), &Evidence::default());
    assert_eq!(s, BuildStatus::Unknown("finished".into()));
    assert!(!s.is_terminal());
}

#[test]
fn dirty_repo_without_evidence_is_stale() {
    let s = resolve(&entry("succeeded", None, true), &Evidence::default());
    assert_eq!(s, BuildStatus::Unknown("unconfirmed: succeeded".into()));
}

/// Seen on every package of a 60-package run: OBS answered `broken: no
/// source uploaded` for minutes after the commit, then built them.
#[test]
fn broken_no_source_uploaded_after_a_commit_is_stale_and_never_accepted() {
    let confirm = Evidence {
        confirm: true,
        ..Evidence::default()
    };
    for dirty in [false, true] {
        let s = resolve(
            &entry("broken", Some("no source uploaded"), dirty),
            &confirm,
        );
        assert_eq!(s, BuildStatus::Unknown("stale: broken".into()));
        assert_eq!(accept_unconfirmed(&s), None);
    }
    // A project-wide scan (no commit, clean repository) takes it at face value.
    assert_eq!(
        resolve_lenient(&entry("broken", Some("no source uploaded"), true)),
        BuildStatus::Broken
    );
    // Any other broken reason is a real verdict.
    assert_eq!(
        resolve(
            &entry("broken", Some("bad build configuration"), false),
            &confirm
        ),
        BuildStatus::Broken
    );
}

#[test]
fn missing_job_info_is_unconfirmed_and_can_be_accepted() {
    let s = resolve(&entry("succeeded", None, true), &Evidence::default());
    assert_eq!(accept_unconfirmed(&s), Some(BuildStatus::Succeeded));
    let f = BuildStatus::Unknown("unconfirmed: failed".into());
    assert_eq!(accept_unconfirmed(&f), Some(BuildStatus::Failed));
    // A job for another revision is stale, never accepted.
    let stale = BuildStatus::Unknown("stale: failed".into());
    assert_eq!(accept_unconfirmed(&stale), None);
    assert_eq!(accept_unconfirmed(&BuildStatus::Building), None);
}

#[test]
fn matching_job_with_a_non_verdict_code_defers_to_the_result() {
    let mut ev = evidence("abc", "abc", "unchanged");
    ev.confirm = true;
    assert_eq!(
        resolve(&entry("succeeded", None, false), &ev),
        BuildStatus::Succeeded
    );
    ev.job.as_mut().unwrap().code = "meta change".into();
    assert_eq!(
        resolve(&entry("failed", None, false), &ev),
        BuildStatus::Failed
    );
}

#[test]
fn dirty_repo_with_matching_job_uses_the_jobs_verdict() {
    let e = entry("finished", Some("failed"), true);
    assert_eq!(
        resolve(&e, &evidence("abc", "abc", "succeeded")),
        BuildStatus::Succeeded
    );
    assert_eq!(
        resolve(&e, &evidence("abc", "abc", "failed")),
        BuildStatus::Failed
    );
}

#[test]
fn dirty_repo_with_job_for_an_older_revision_stays_stale() {
    let e = entry("finished", Some("failed"), true);
    let s = resolve(&e, &evidence("new", "old", "failed"));
    assert_eq!(s, BuildStatus::Unknown("stale: failed".into()));
}

#[test]
fn verifymd5_counts_as_the_same_revision() {
    let src = SourceInfo {
        srcmd5: "expanded".into(),
        verifymd5: Some("verify".into()),
    };
    let job = |srcmd5: &str, v: Option<&str>| JobInfo {
        srcmd5: srcmd5.into(),
        verifymd5: v.map(str::to_string),
        code: "succeeded".into(),
    };
    assert!(job("verify", None).is_for(&src));
    assert!(job("other", Some("verify")).is_for(&src));
    assert!(!job("other", Some("different")).is_for(&src));
    assert!(!job("other", None).is_for(&src));
}

#[test]
fn disabled_and_excluded_are_never_stale() {
    let none = Evidence::default();
    assert_eq!(
        resolve(&entry("disabled", None, true), &none),
        BuildStatus::Disabled
    );
    assert_eq!(
        resolve(&entry("excluded", None, true), &none),
        BuildStatus::Excluded
    );
    assert!(!needs_evidence(&entry("excluded", None, true), false));
}

/// python-nvidia-cutlass-dsl-libs-base as OBS reported it: excluded on
/// i586, succeeded on x86_64, in one document. Each pair resolves to its
/// own state whichever order the document lists them in, and the x86_64
/// `building` of the repository itself does not leak into the package.
#[test]
fn each_repository_arch_resolves_to_its_own_status() {
    let xml = r#"<resultlist>
  <result project="p" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-nvidia-cutlass-dsl-libs-base" code="excluded"/>
  </result>
  <result project="p" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building">
    <status package="python-nvidia-cutlass-dsl-libs-base" code="succeeded"/>
  </result>
</resultlist>"#;
    let status = |arch: &str| {
        let e = find_entry(
            xml,
            "openSUSE_Tumbleweed",
            arch,
            "python-nvidia-cutlass-dsl-libs-base",
        )
        .unwrap();
        resolve(&e, &Evidence::default())
    };
    assert_eq!(status("x86_64"), BuildStatus::Succeeded);
    assert_eq!(status("i586"), BuildStatus::Excluded);
    assert!(
        find_entry(
            xml,
            "openSUSE_Tumbleweed",
            "aarch64",
            "python-nvidia-cutlass-dsl-libs-base"
        )
        .is_none()
    );
}

#[test]
fn binary_sizes_skip_source_rpms_and_non_rpms() {
    let listing = r#"<binarylist>
  <binary filename="_buildenv" size="348736" mtime="1791824718"/>
  <binary filename="python-cuda-bindings-13.4.3-2.1.src.rpm" size="3285054" mtime="1791824718"/>
  <binary filename="python313-cuda-bindings-13.4.3-2.1.x86_64.rpm" size="11053" mtime="1791824718"/>
  <binary filename="python314-cuda-bindings-13.4.3-2.1.x86_64.rpm" size="10925" mtime="1791824718"/>
  <binary filename="rpmlint.log" size="2502" mtime="1791824717"/>
</binarylist>"#;
    assert_eq!(
        parse_binary_rpm_sizes(listing),
        vec![
            (
                "python313-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
                11053
            ),
            (
                "python314-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
                10925
            ),
        ]
    );
}

#[test]
fn unresolvable_details_stay_with_their_own_result() {
    let xml = r#"<resultlist>
  <result repository="r" arch="i586">
    <status package="p" code="unresolvable"><details>nothing provides python314-fastar, nothing provides python313-fastar</details></status>
  </result>
  <result repository="r" arch="x86_64">
    <status package="p" code="succeeded"/>
  </result>
</resultlist>"#;
    let i586 = find_entry(xml, "r", "i586", "p").unwrap();
    assert_eq!(
        i586.details.as_deref(),
        Some("nothing provides python314-fastar, nothing provides python313-fastar")
    );
    assert_eq!(find_entry(xml, "r", "x86_64", "p").unwrap().details, None);
}

#[test]
fn finished_unchanged_is_a_success_that_needs_no_new_job() {
    let e = entry("finished", Some("unchanged"), false);
    let with_confirm = Evidence {
        confirm: true,
        ..Default::default()
    };
    assert_eq!(resolve(&e, &with_confirm), BuildStatus::Succeeded);
    assert!(!needs_evidence(&e, true));
}

#[test]
fn flavors_are_read_from_their_own_repository_and_arch() {
    let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="foo" code="succeeded"/>
    <status package="foo:py313" code="failed"/>
    <status package="foobar" code="failed"/>
  </result>
  <result repository="r" arch="i586"><status package="foo:py314" code="failed"/></result>
</resultlist>"#;
    let f = flavor_entries(xml, "r", "x86_64", "foo");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].pkg, "foo:py313");
}

#[test]
fn a_package_waits_for_its_flavors_and_fails_with_any_of_them() {
    use BuildStatus::*;
    assert_eq!(combine(Succeeded, &[Building]), Building);
    assert_eq!(combine(Succeeded, &[Succeeded, Failed]), Failed);
    assert_eq!(combine(Unresolvable, &[Succeeded]), Unresolvable);
    assert_eq!(combine(Failed, &[Unresolvable]), Failed);
    assert_eq!(combine(Succeeded, &[Excluded]), Succeeded);
    assert_eq!(combine(Excluded, &[Succeeded]), Succeeded);
    assert_eq!(combine(Excluded, &[]), Excluded);
    assert_eq!(combine(Scheduled, &[Blocked]), Scheduled);
}

#[test]
fn needs_evidence_only_for_dirty_terminal_results() {
    assert!(needs_evidence(&entry("succeeded", None, true), false));
    assert!(needs_evidence(
        &entry("finished", Some("failed"), true),
        false
    ));
    assert!(!needs_evidence(&entry("succeeded", None, false), false));
    assert!(!needs_evidence(&entry("building", None, true), false));
}

#[test]
fn after_a_commit_a_clean_built_verdict_needs_the_job_history() {
    assert!(needs_evidence(&entry("failed", None, false), true));
    assert!(needs_evidence(&entry("succeeded", None, false), true));
    // Scheduler judgements are not in the job history.
    assert!(!needs_evidence(&entry("unresolvable", None, false), true));
    assert!(!needs_evidence(&entry("excluded", None, false), true));
    assert!(!needs_evidence(&entry("building", None, false), true));
}

#[test]
fn after_a_commit_the_previous_revisions_verdict_is_stale_even_when_clean() {
    let e = entry("failed", None, false);
    let confirm = |mut ev: Evidence| {
        ev.confirm = true;
        ev
    };
    // The newest job is for the old sources.
    assert_eq!(
        resolve(&e, &confirm(evidence("new", "old", "failed"))),
        BuildStatus::Unknown("stale: failed".into())
    );
    // No evidence at all is not a confirmation.
    assert_eq!(
        resolve(&e, &confirm(Evidence::default())),
        BuildStatus::Unknown("unconfirmed: failed".into())
    );
    // The job for the new sources decides.
    assert_eq!(
        resolve(&e, &confirm(evidence("new", "new", "succeeded"))),
        BuildStatus::Succeeded
    );
    // Not asked to confirm: the clean verdict stands.
    assert_eq!(
        resolve(&e, &evidence("new", "old", "failed")),
        BuildStatus::Failed
    );
}

#[test]
fn lenient_resolution_ignores_dirty() {
    assert_eq!(
        resolve_lenient(&entry("failed", None, true)),
        BuildStatus::Failed
    );
}

#[test]
fn latest_job_is_the_last_entry_and_source_info_parses() {
    let hist = r#"<jobhistlist>
  <jobhist package="p" rev="11" srcmd5="aaa" code="failed" verifymd5="aaa"/>
  <jobhist package="p" rev="12" srcmd5="bbb" code="succeeded"/>
</jobhistlist>"#;
    let j = parse_latest_job(hist).unwrap();
    assert_eq!((j.srcmd5.as_str(), j.code.as_str()), ("bbb", "succeeded"));
    assert!(parse_latest_job("<jobhistlist></jobhistlist>").is_none());
    let src = parse_source_info(
            r#"<sourceinfo package="p" rev="12" srcmd5="bbb" verifymd5="ccc"><filename>p.spec</filename></sourceinfo>"#,
        )
        .unwrap();
    assert_eq!(
        (src.srcmd5.as_str(), src.verifymd5.as_deref()),
        ("bbb", Some("ccc"))
    );
}

#[test]
fn binary_list_yields_binary_rpms_only() {
    let xml = r#"<binarylist>
  <binary filename="_buildenv" size="1" mtime="1"/>
  <binary filename="python313-Events-0.5-2.1.noarch.rpm" size="1" mtime="1"/>
  <binary filename="python-events-0.5-2.1.src.rpm" size="1" mtime="1"/>
  <binary filename="buildinfo" size="1" mtime="1"/>
</binarylist>"#;
    assert_eq!(
        parse_binary_rpms(xml),
        ["python313-Events-0.5-2.1.noarch.rpm"]
    );
}

fn unknown(s: &str) -> BuildStatus {
    BuildStatus::Unknown(s.to_string())
}

#[test]
fn stale_and_unconfirmed_await_a_new_revision() {
    assert!(awaiting_new_revision(&unknown("stale: failed")));
    assert!(awaiting_new_revision(&unknown(
        "stale: outdated (was: blocked)"
    )));
    assert!(awaiting_new_revision(&unknown("unconfirmed: broken")));
    assert!(!awaiting_new_revision(&unknown("finished")));
    assert!(!awaiting_new_revision(&unknown("no results")));
    assert!(!awaiting_new_revision(&BuildStatus::Failed));
    assert!(!awaiting_new_revision(&BuildStatus::Building));
}

#[test]
fn a_table_verdict_never_settles_a_stale_status() {
    // The previous revision's `failed` is what a clean repository's table
    // shows right after a commit: it must not become the new verdict.
    for api in ["stale: failed", "stale: succeeded", "unconfirmed: broken"] {
        for table in [
            BuildStatus::Failed,
            BuildStatus::Succeeded,
            BuildStatus::Broken,
        ] {
            assert_eq!(cross_check(&unknown(api), &table), CrossCheck::Ignore);
        }
    }
    assert_eq!(
        cross_check(&unknown("stale: failed"), &unknown("no results")),
        CrossCheck::Ignore
    );
}

#[test]
fn a_scheduler_state_in_the_table_means_the_new_revision_was_taken() {
    for table in [
        BuildStatus::Scheduled,
        BuildStatus::Blocked,
        BuildStatus::Building,
        unknown("stale: outdated (was: blocked: ffmpeg-8-mini-libs)"),
    ] {
        assert_eq!(
            cross_check(&unknown("stale: succeeded"), &table),
            CrossCheck::NewRevisionSeen
        );
    }
}

#[test]
fn a_table_that_moved_past_a_live_status_replaces_it() {
    assert_eq!(
        cross_check(&BuildStatus::Building, &BuildStatus::Succeeded),
        CrossCheck::Adopt
    );
    assert_eq!(
        cross_check(
            &BuildStatus::Blocked,
            &unknown("stale: outdated (was: blocked)")
        ),
        CrossCheck::Adopt
    );
    assert_eq!(
        cross_check(&BuildStatus::Blocked, &BuildStatus::Blocked),
        CrossCheck::Agrees
    );
    assert_eq!(
        cross_check(&unknown("stale: failed"), &unknown("stale: failed")),
        CrossCheck::Agrees
    );
}

#[test]
fn parsed_entries_serve_the_package_and_its_flavors() {
    let xml = r#"<resultlist>
  <result project="p" repository="r" arch="a">
    <status package="foo" code="succeeded"/>
    <status package="foo:mini" code="failed"/>
    <status package="foobar" code="building"/>
  </result>
  <result project="p" repository="r" arch="b">
    <status package="foo" code="blocked"/>
  </result>
</resultlist>"#;
    let entries = parse_results(xml);
    assert_eq!(
        entry_in(&entries, "r", "a", "foo").unwrap().code,
        "succeeded"
    );
    assert_eq!(entry_in(&entries, "r", "b", "foo").unwrap().code, "blocked");
    assert!(entry_in(&entries, "r", "a", "foo:mini").is_some());
    assert!(entry_in(&entries, "r", "c", "foo").is_none());
    let flavors = flavors_in(&entries, "r", "a", "foo");
    assert_eq!(flavors.len(), 1);
    assert_eq!(flavors[0].pkg, "foo:mini");
    assert_eq!(
        find_entry(xml, "r", "a", "foo"),
        entry_in(&entries, "r", "a", "foo").cloned()
    );
    assert_eq!(flavor_entries(xml, "r", "a", "foo"), flavors);
}
