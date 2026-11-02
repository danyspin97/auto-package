use super::*;

/// `osc api /source/X11:Wayland/stacktile?expand=0`, trimmed to the link.
const STACKTILE_LISTING: &str = r#"<directory name="stacktile" rev="4" vrev="4" srcmd5="3af17805ec7fc12bbd77219054912011">
  <linkinfo project="openSUSE:Factory" package="stacktile" baserev="bf9f65a2126dd56ffc8ae5920f9e4188" error="openSUSE:Factory/stacktile: package 'stacktile' does not exist"/>
  <entry name="_link" md5="f9a820ddd17b5569eb67ac274bc87692" size="124" mtime="1730024772"/>
</directory>"#;

/// `/search/request` for the accepted deletes of openSUSE:Factory/stacktile:
/// the request's own description comes after its reviews, whose histories
/// carry descriptions too.
const STACKTILE_DELETE: &str = r#"<collection matches="1">
  <request id="1347021" creator="repo-checker">
    <action type="delete">
      <target project="openSUSE:Factory" package="stacktile"/>
    </action>
    <state name="accepted" who="dimstar_suse" when="2026-04-17T18:56:14" created="2026-04-14T22:03:12">
      <comment>Staging Project openSUSE:Factory:Staging:adi:94 got accepted.</comment>
    </state>
    <review state="accepted" when="2026-04-14T22:06:26" who="licensedigger" by_user="licensedigger">
      <comment/>
      <history who="licensedigger" when="2026-04-14T22:06:26">
        <description>Review got accepted</description>
      </history>
    </review>
    <description>[botdel] Package has had build problems for &gt;= 6 weeks</description>
  </request>
</collection>"#;

#[test]
fn the_link_target_is_read_from_the_listing() {
    assert_eq!(
        parse_linkinfo(STACKTILE_LISTING, "stacktile"),
        Some(("openSUSE:Factory".into(), "stacktile".into()))
    );
    let renamed = r#"<linkinfo project="openSUSE:Factory" package="wlroots-0_16"/>"#;
    assert_eq!(
        parse_linkinfo(renamed, "wlroots"),
        Some(("openSUSE:Factory".into(), "wlroots-0_16".into()))
    );
    // Without a linkinfo there is no target to report.
    assert_eq!(parse_linkinfo("<directory name=\"p\"/>", "p"), None);
}

#[test]
fn the_request_id_date_and_own_description_are_read() {
    let reqs = parse_accepted_deletes(STACKTILE_DELETE);
    assert_eq!(
        reqs,
        vec![DeleteRequest {
            id: "1347021".into(),
            accepted: "2026-04-17".into(),
            description: "[botdel] Package has had build problems for >= 6 weeks".into(),
            targets: vec![("openSUSE:Factory".into(), "stacktile".into())],
        }]
    );
}

#[test]
fn a_request_that_is_not_accepted_is_not_a_delete_to_report() {
    let declined = STACKTILE_DELETE.replace("state name=\"accepted\"", "state name=\"declined\"");
    assert!(parse_accepted_deletes(&declined).is_empty());
}

#[test]
fn only_a_delete_of_the_target_itself_counts() {
    assert!(accepted_delete_request(STACKTILE_DELETE, "openSUSE:Factory", "wayprompt").is_none());
    let other_action =
        STACKTILE_DELETE.replace("<action type=\"delete\">", "<action type=\"submit\">");
    assert!(accepted_delete_request(&other_action, "openSUSE:Factory", "stacktile").is_none());
}

#[test]
fn the_latest_accepted_delete_is_the_one_reported() {
    let older = STACKTILE_DELETE
        .replace("1347021", "1100000")
        .replace("2026-04-17T18:56:14", "2024-01-02T10:00:00");
    let xml = format!(
        "{}{}",
        older.trim_end_matches("</collection>"),
        STACKTILE_DELETE.trim_start_matches("<collection matches=\"1\">")
    );
    let found = accepted_delete_request(&xml, "openSUSE:Factory", "stacktile").unwrap();
    assert_eq!(found.id, "1347021");
}

#[test]
fn a_broken_link_names_the_request_that_deleted_its_target() {
    let report = broken_link_report_with("X11:Wayland", "stacktile", "fallback", &|path| {
        Ok(if path.contains("expand=0") {
            STACKTILE_LISTING.to_string()
        } else {
            assert!(path.starts_with("/search/request?match="), "{path}");
            STACKTILE_DELETE.to_string()
        })
    });
    assert!(
        report.starts_with(
            "X11:Wayland/stacktile links to openSUSE:Factory/stacktile, deleted from \
             openSUSE:Factory by request #1347021 (2026-04-17)"
        ),
        "{report}"
    );
    assert!(
        report.contains("unlink it (replace the link with files)"),
        "{report}"
    );
}

#[test]
fn a_broken_link_without_an_accepted_delete_names_only_the_missing_target() {
    let report = broken_link_report_with("X11:Wayland", "wayprompt", "fallback", &|path| {
        Ok(if path.contains("expand=0") {
            r#"<linkinfo project="openSUSE:Factory" package="wayprompt" error="openSUSE:Factory/wayprompt: package 'wayprompt' does not exist"/>"#.to_string()
        } else {
            "<collection matches=\"0\"/>".to_string()
        })
    });
    assert!(
        report.contains("no accepted delete request for openSUSE:Factory/wayprompt was found"),
        "{report}"
    );
    assert!(!report.contains("fallback"), "{report}");
}

#[test]
fn a_link_that_cannot_be_read_keeps_the_plain_reason() {
    let report = broken_link_report_with("X11:Wayland", "p", "the plain reason", &|_| {
        Err("HTTP Error 503: Service Unavailable".into())
    });
    assert_eq!(report, "the plain reason");
}

#[test]
fn a_failed_request_search_is_said_not_claimed_as_no_delete() {
    let report = broken_link_report_with("X11:Wayland", "p", "fallback", &|path| {
        if path.contains("expand=0") {
            Ok(r#"<linkinfo project="openSUSE:Factory" package="p" error="openSUSE:Factory/p: package 'p' does not exist"/>"#.to_string())
        } else {
            Err("HTTP Error 500".into())
        }
    });
    assert!(report.contains("could not be read"), "{report}");
    assert!(!report.contains("no accepted delete request"), "{report}");
}

#[test]
fn a_link_in_conflict_is_reported_with_its_error_and_no_search_is_made() {
    let listing =
        r#"<linkinfo project="openSUSE:Factory" package="p" error="conflict in file p.spec"/>"#;
    let report = broken_link_report_with("X11:Wayland", "p", "fallback", &|path| {
        assert!(path.contains("expand=0"), "{path}");
        Ok(listing.to_string())
    });
    assert!(
        report.contains("cannot be expanded: conflict in file p.spec"),
        "{report}"
    );
    assert!(!report.contains("does not exist"), "{report}");
    assert!(!report.contains("delete"), "{report}");
}

#[test]
fn a_link_without_an_error_keeps_the_plain_reason_and_no_search_is_made() {
    let report = broken_link_report_with("X11:Wayland", "p", "the plain reason", &|path| {
        assert!(path.contains("expand=0"), "{path}");
        Ok(r#"<linkinfo project="openSUSE:Factory" package="p"/>"#.to_string())
    });
    assert_eq!(report, "the plain reason");
}

#[test]
fn a_target_name_that_is_not_an_obs_name_is_not_searched() {
    let listing = r#"<linkinfo project="openSUSE:Factory" package="x' or '1'='1" error="openSUSE:Factory/x: package 'x' does not exist"/>"#;
    let report = broken_link_report_with("X11:Wayland", "p", "the plain reason", &|path| {
        assert!(path.contains("expand=0"), "searched {path}");
        Ok(listing.to_string())
    });
    assert_eq!(report, "the plain reason");
    assert!(delete_requests_path("openSUSE:Factory", "x' or 'a'='a").is_none());
    assert!(delete_requests_path("openSUSE:Factory", "stacktile").is_some());
}

/// The request's own description, decoded: entities and numeric references
/// in the text, and CDATA sections, come out as the characters they stand for.
fn description_of(text: &str) -> String {
    let xml = format!(
        "<collection matches=\"1\"><request id=\"7\">\
         <action type=\"delete\"><target project=\"P\" package=\"x\"/></action>\
         <state name=\"accepted\" when=\"2026-01-02T03:04:05\"/>\
         <description>{text}</description></request></collection>"
    );
    let found = parse_accepted_deletes(&xml);
    assert_eq!(found.len(), 1);
    found[0].description.clone()
}

#[test]
fn a_description_is_read_with_its_entities_decoded() {
    assert_eq!(
        description_of("[botdel] build problems for &gt;= 6 weeks &amp; more"),
        "[botdel] build problems for >= 6 weeks & more"
    );
    assert_eq!(
        description_of("&lt;b&gt; &quot;q&quot; &apos;a&apos; &#62;n &#x26;x"),
        "<b> \"q\" 'a' >n &x"
    );
}

#[test]
fn a_description_is_read_with_its_cdata_sections_as_written() {
    assert_eq!(
        description_of("see <![CDATA[<b> & x &gt; y]]> now"),
        "see <b> & x &gt; y now"
    );
}

#[test]
fn the_report_prints_a_request_description_decoded() {
    let report = broken_link_report_with("X11:Wayland", "stacktile", "fallback", &|path| {
        Ok(if path.contains("expand=0") {
            STACKTILE_LISTING.to_string()
        } else {
            STACKTILE_DELETE.replace(
                "[botdel] Package has had build problems for &gt;= 6 weeks",
                "[botdel] build problems for &gt;= 6 weeks &amp; more",
            )
        })
    });
    assert!(
        report.contains(": \"[botdel] build problems for >= 6 weeks & more\""),
        "{report}"
    );
    assert!(!report.contains("&gt;"), "{report}");
}
