use super::*;

use super::helpers::*;

#[test]
pub(super) fn yes_blocks_a_package_whose_recorded_risks_need_a_review_or_cannot_be_read() {
    use crate::pipeline::PkgState;
    use crate::risk_flags::{RiskFlag, settle};
    use crate::workspace::Workspace;
    let root = tempfile::tempdir().unwrap();
    let roots =
        |pkg_root: &std::path::Path| vec![("home:u:Base".to_string(), pkg_root.to_path_buf())];
    // Never built in this branch: the state is missing, which blocks too.
    let missing = super::yes_blockers(&roots(&root.path().join("nothing")), "python-foo");
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains("no recorded state"), "{missing:?}");
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| {
        settle(
            st,
            "r1",
            vec![RiskFlag::NewScriptlet {
                header: "%post".into(),
            }],
        )
    })
    .unwrap();
    assert_eq!(
        super::yes_blockers(&roots(root.path()), "python-foo"),
        vec!["RISK: new scriptlet: %post".to_string()]
    );
    std::fs::write(ws.dir().join("state.json"), "{ not json").unwrap();
    let lines = super::yes_blockers(&roots(root.path()), "python-foo");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("cannot be read"), "{lines:?}");
}

#[test]
pub(super) fn maintained_query_is_the_dry_run_osc_maintained_makes() {
    assert_eq!(
        maintained_api_path("libsigc++"),
        "/source?cmd=branch&attribute=OBS%3AMaintained&update_project_attribute=OBS%3AUpdateProject&dryrun=1&package=libsigc%2B%2B"
    );
}

#[test]
pub(super) fn maintained_collection_is_parsed_with_devel_and_incident_names() {
    let xml = r#"<collection>
  <package project="openSUSE:Backports:SLE-15-SP7:Update" package="Botan">
    <target project="home:u:branches:OBS_Maintained:Botan" package="Botan.openSUSE:Backports:SLE-15-SP7:Update"/>
  </package>
  <package project="SUSE:SLE-15-SP6:Update" package="Botan.36123"/>
  <package project="openSUSE:Leap:15.6:Update" package="Botan">
    <devel project="SUSE:SLE-15-SP6:Update" package="Botan"/>
  </package>
</collection>"#;
    let got = parse_maintained(xml);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].project, "openSUSE:Backports:SLE-15-SP7:Update");
    assert_eq!(got[0].package, "Botan");
    assert_eq!(got[0].devel, None);
    assert_eq!(got[1].package, "Botan");
    assert_eq!(
        got[2].devel,
        Some(("SUSE:SLE-15-SP6:Update".into(), "Botan".into()))
    );
    assert!(parse_maintained("<collection/>").is_empty());
}

#[test]
pub(super) fn osc_maintained_text_is_parsed_and_git_lines_skipped() {
    let text = "openSUSE:Backports:SLE-15-SP7:Update/Botan\n\
                    openSUSE:Leap:15.6:Update/curl using sources from SUSE:SLE-15-SP6:Update/curl\n\
                    [git] pool/curl:leap-16.0\n";
    let got = parse_maintained_text(text);
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].project, "openSUSE:Leap:15.6:Update");
    assert_eq!(
        got[1].devel,
        Some(("SUSE:SLE-15-SP6:Update".into(), "curl".into()))
    );
    assert!(says_nothing_maintained(
        "BuildService API error: no packages found by search criteria"
    ));
}

#[test]
pub(super) fn maintenance_project_comes_from_the_attribute_search() {
    assert_eq!(
        maintenance_project_search_path(),
        "/search/project/id?match=attribute%2F%40name%3D%27OBS%3AMaintenanceProject%27"
    );
    let xml = "<collection matches=\"1\">\n  <project name='openSUSE:Maintenance'/>\n</collection>";
    assert_eq!(
        parse_first_project(xml).as_deref(),
        Some("openSUSE:Maintenance")
    );
    assert_eq!(parse_first_project("<collection matches=\"0\"/>"), None);
}

#[test]
pub(super) fn codestreams_are_picked_by_name_short_name_or_all_and_others_refused() {
    let available = vec![cs("SUSE:SLE-15-SP7:Update"), cs("SUSE:SLE-15-SP6:Update")];
    let got = select_codestreams(&available, &["SUSE:SLE-15-SP6".into()]).unwrap();
    assert_eq!(got, vec![cs("SUSE:SLE-15-SP6:Update")]);
    assert_eq!(
        select_codestreams(&available, &["all".into()]).unwrap(),
        available
    );
    let err = select_codestreams(&available, &["SUSE:SLE-12-SP5:Update".into()]).unwrap_err();
    assert!(
        err.contains("SUSE:SLE-12-SP5:Update does not maintain"),
        "{err}"
    );
    assert!(
        err.contains("SUSE:SLE-15-SP7:Update, SUSE:SLE-15-SP6:Update"),
        "{err}"
    );
    assert!(select_codestreams(&available, &[]).is_err());
}

#[test]
pub(super) fn common_codestreams_are_those_of_every_package() {
    let a = vec![cs("A:Update"), cs("B:Update")];
    let b = vec![cs("B:Update"), cs("C:Update")];
    assert_eq!(
        common_codestreams(&[a.clone(), b]),
        vec!["B:Update".to_string()]
    );
    assert_eq!(
        common_codestreams(&[a]),
        vec!["A:Update".to_string(), "B:Update".to_string()]
    );
    assert!(common_codestreams(&[]).is_empty());
}

#[test]
pub(super) fn a_typed_pick_takes_numbers_names_or_all() {
    let p = vec!["A:Update".to_string(), "B:Update".to_string()];
    assert_eq!(pick_lines(&p), vec!["  1) A:Update", "  2) B:Update"]);
    assert_eq!(parse_pick("2", &p).unwrap(), vec!["B:Update"]);
    assert_eq!(parse_pick("1, B", &p).unwrap(), p);
    assert_eq!(parse_pick("ALL", &p).unwrap(), p);
    assert!(parse_pick("3", &p).is_err());
    assert!(parse_pick("  ", &p).is_err());
    assert!(parse_pick("C", &p).is_err());
}

#[test]
pub(super) fn the_newest_incident_copy_has_the_highest_number() {
    let listing = [
        "Botan",
        "Botan.19461",
        "Botan.2000",
        "Botan-doc.99999",
        "Botan.x1",
        "curl.5",
    ];
    assert_eq!(
        newest_incident_in(listing, "Botan").as_deref(),
        Some("Botan.19461")
    );
    assert_eq!(newest_incident_in(["Botan"], "Botan"), None);
    assert_eq!(without_incident_suffix("Botan.19461"), "Botan");
    assert_eq!(
        without_incident_suffix("python-ruamel.yaml"),
        "python-ruamel.yaml"
    );
}

#[test]
pub(super) fn a_package_without_an_update_is_browsed_in_the_linked_project() {
    let meta = r#"<project name="SUSE:SLE-15-SP7:Update" kind="maintenance_release">
  <link project="SUSE:SLE-15-SP7:GA"/>
  <repository name="standard">
    <path project="SUSE:SLE-15-SP7:GA" repository="standard"/>
  </repository>
</project>"#;
    assert_eq!(parse_project_links(meta), vec!["SUSE:SLE-15-SP7:GA"]);
    assert_eq!(
        planning_source("SUSE:SLE-15-SP7:Update", true, meta).as_deref(),
        Some("SUSE:SLE-15-SP7:Update")
    );
    assert_eq!(
        planning_source("SUSE:SLE-15-SP7:Update", false, meta).as_deref(),
        Some("SUSE:SLE-15-SP7:GA")
    );
    assert_eq!(
        planning_source("X:Update", false, "<project name=\"X:Update\"/>"),
        None
    );
    assert!(source_kind_allowed("maintenance_release"));
    assert!(!source_kind_allowed("maintenance_incident"));
}

#[test]
pub(super) fn branches_are_named_like_osc_branch_and_cleaned_up_only_in_home_branches() {
    assert_eq!(
        branch_project_for("u", "SUSE:SLE-15-SP7:Update"),
        "home:u:branches:SUSE:SLE-15-SP7:Update"
    );
    assert_eq!(
        sourceupdate_for("home:u:branches:SUSE:SLE-15-SP7:Update", Some("u")),
        Some("cleanup")
    );
    assert_eq!(sourceupdate_for("home:u:work", Some("u")), None);
    assert_eq!(sourceupdate_for("home:u:branches:X", None), None);
}

#[test]
pub(super) fn bug_references_are_checked_and_spelled_the_suse_way() {
    assert_eq!(parse_bug_ref("BSC#1234567").unwrap(), "bsc#1234567");
    assert_eq!(parse_bug_ref("boo#123456").unwrap(), "boo#123456");
    assert_eq!(parse_bug_ref("jsc#sle-12345").unwrap(), "jsc#SLE-12345");
    assert_eq!(parse_bug_ref("jsc#PED-1234").unwrap(), "jsc#PED-1234");
    assert_eq!(parse_bug_ref("cve-2025-12345").unwrap(), "CVE-2025-12345");
    for bad in [
        "bsc#12",
        "bsc1234567",
        "CVE-25-1234",
        "CVE-2025-12",
        "jsc#123",
        "gh#a/b#1",
        "",
    ] {
        assert!(parse_bug_ref(bad).is_err(), "{bad}");
    }
}

#[test]
pub(super) fn a_maintenance_request_needs_a_reference_unless_told_otherwise() {
    let got = parse_bug_refs(
        &["bsc#1234567,CVE-2025-1234".into(), "BSC#1234567".into()],
        false,
    )
    .unwrap();
    assert_eq!(got, vec!["bsc#1234567", "CVE-2025-1234"]);
    assert!(
        parse_bug_refs(&[], false)
            .unwrap_err()
            .contains("--no-bug-ref")
    );
    assert!(parse_bug_refs(&[], true).unwrap().is_empty());
    assert!(parse_bug_refs(&["bsc#1".into()], true).is_err());
    assert_eq!(bug_ref_warnings(&["CVE-2025-1234".into()]).len(), 1);
    assert!(bug_ref_warnings(&["CVE-2025-1234".into(), "bsc#1234567".into()]).is_empty());
    assert_eq!(refs_suffix(&got), "(bsc#1234567, CVE-2025-1234)");
}

#[test]
pub(super) fn references_go_to_the_end_of_the_first_bullet_before_its_colon() {
    let refs = vec!["bsc#1234567".to_string(), "CVE-2025-1234".to_string()];
    let got = with_refs_in_newest_entry(CHANGES, &refs).unwrap();
    assert!(
        got.contains("- Update to 1.2.3 (bsc#1234567, CVE-2025-1234):\n  * fix a crash"),
        "{got}"
    );
    assert!(got.ends_with("- Update to 1.2.2 (bsc#1111111)\n"));
    // Idempotent: a second run adds nothing.
    assert_eq!(with_refs_in_newest_entry(&got, &refs), None);
    // Only the missing ones are added, and an older entry does not count.
    let got = with_refs_in_newest_entry(CHANGES, &["bsc#1111111".into()]).unwrap();
    assert!(got.contains("- Update to 1.2.3 (bsc#1111111):"), "{got}");
}

#[test]
pub(super) fn references_follow_a_wrapped_bullet_and_an_entry_without_one_gets_its_own() {
    let wrapped = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

- Fix the parser so that a long line
  no longer crashes it
- Second change
";
    let got = with_refs_in_newest_entry(wrapped, &["bsc#1234567".into()]).unwrap();
    assert!(
        got.contains("  no longer crashes it (bsc#1234567)\n- Second change"),
        "{got}"
    );
    let bare = "-------------------------------------------------------------------
Mon Oct 26 10:00:00 UTC 2026 - Dev <dev@suse.com>

Rebuilt.
";
    let got = with_refs_in_newest_entry(bare, &["bsc#1234567".into()]).unwrap();
    assert!(
        got.contains("<dev@suse.com>\n- References: bsc#1234567\n"),
        "{got}"
    );
    assert_eq!(
        with_refs_in_newest_entry("no entries", &["bsc#1234567".into()]),
        None
    );
}

#[test]
pub(super) fn a_reference_counts_only_as_a_whole() {
    assert!(mentions("fixed (bsc#123456)", "bsc#123456"));
    assert!(!mentions("fixed (bsc#1234567)", "bsc#123456"));
    assert!(mentions("CVE-2025-1234, x", "cve-2025-1234"));
    assert_eq!(
        missing_refs("bsc#1234567", &["bsc#1234567".into(), "CVE-2025-1".into()]),
        vec!["CVE-2025-1"]
    );
}
