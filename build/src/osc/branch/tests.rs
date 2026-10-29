
use super::*;

#[test]
fn repositories_without_a_block_get_a_local_one() {
    let xml = "<project>\n  <repository name=\"a\">\n    <arch>x86_64</arch>\n  </repository>\n  \
                   <repository name=\"b\" block=\"all\"/>\n  <repository name=\"c\"/>\n</project>";
    let edited = with_local_blocking(xml).unwrap();
    assert!(edited.contains("<repository name=\"a\" block=\"local\">"));
    assert!(edited.contains("<repository name=\"b\" block=\"all\"/>"));
    assert!(edited.contains("<repository name=\"c\" block=\"local\"/>"));
    assert_eq!(with_local_blocking(&edited), None);
}

#[test]
fn linkinfo_behind_only_when_base_differs_from_origin() {
    let at = |src: &str, base: &str| {
        format!(r#"<directory><linkinfo project="P" srcmd5="{src}" baserev="{base}"/></directory>"#)
    };
    assert!(!linkinfo_is_behind(&at("aa", "aa")));
    assert!(linkinfo_is_behind(&at("bb", "aa")));
    assert!(linkinfo_is_behind("<directory/>"));
}

const META: &str = r#"<project name="home:u:branches:Base:System">
  <title>Branch</title>
  <description/>
  <person userid="u" role="maintainer"/>
  <repository name="openSUSE_Tumbleweed">
    <path project="Base:System" repository="openSUSE_Tumbleweed"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
  <repository name="15.6">
    <path project="Base:System" repository="15.6"/>
    <path project="openSUSE:Factory" repository="snapshot"/>
    <arch>x86_64</arch>
  </repository>
</project>
"#;

pub(super) fn add(repo: &str, project: &str, path_repo: &str) -> PathAddition {
    PathAddition {
        repository: repo.into(),
        project: project.into(),
        path_repository: path_repo.into(),
    }
}

#[test]
fn a_broken_link_shows_in_the_linkinfo_error() {
    let ok = r#"<directory name="p"><linkinfo project="o" package="p" srcmd5="a" xsrcmd5="b"/></directory>"#;
    assert_eq!(linkinfo_error(ok), None);
    let bad = r#"<directory name="p"><linkinfo project="o" package="p" error="conflict in file p.spec"/></directory>"#;
    assert_eq!(
        linkinfo_error(bad).as_deref(),
        Some("conflict in file p.spec")
    );
    assert_eq!(linkinfo_error("<directory/>"), None);
}

#[test]
fn whoami_reads_a_bare_name_or_a_labelled_one() {
    assert_eq!(parse_whoami("danyspin97\n").as_deref(), Some("danyspin97"));
    assert_eq!(parse_whoami("Username: dany\n").as_deref(), Some("dany"));
    assert_eq!(
        parse_whoami("dspinella: \"Danilo Spinella\" <d@example.org>\n").as_deref(),
        Some("dspinella"),
        "the osc whois form"
    );
    assert_eq!(parse_whoami(""), None);
    assert_eq!(parse_whoami("-rf\n"), None);
}

#[test]
fn default_branch_project_is_what_osc_branch_uses() {
    assert_eq!(
        default_branch_project("dany", "Base:System"),
        "home:dany:branches:Base:System"
    );
}

#[test]
fn branch_path_names_source_package_and_target() {
    assert_eq!(
        branch_api_path(
            "Base:System",
            "libsigc++",
            "home:u:branches:Base:System",
            "a b",
            false
        ),
        "/source/Base:System/libsigc%2B%2B?cmd=branch\
             &target_project=home%3Au%3Abranches%3ABase%3ASystem\
             &target_package=libsigc%2B%2B&ignoredevel=1&comment=a%20b"
    );
}

#[test]
fn branch_path_can_name_the_branch_differently_from_the_source() {
    let path = branch_api_path_as(
        "Base",
        "python-APScheduler",
        "python-apscheduler",
        "home:u",
        "c",
        false,
    );
    assert!(
        path.starts_with("/source/Base/python-APScheduler?cmd=branch&target_project=home%3Au&"),
        "{path}"
    );
    assert!(
        path.contains("&target_package=python-apscheduler&ignoredevel=1&"),
        "{path}"
    );
}

#[test]
fn a_revision_is_sent_as_the_rev_parameter() {
    let at = |rev| branch_api_path_at("S", "p", "p", "T", "c", false, rev);
    assert_eq!(
        at(Some("7")),
        "/source/S/p?cmd=branch&target_project=T&target_package=p&ignoredevel=1&rev=7&comment=c"
    );
    let md5 = "0123456789abcdef0123456789abcdef";
    assert!(at(Some(md5)).contains(&format!("&rev={md5}&comment=")));
    // Without one the request is what it always was.
    assert_eq!(at(None), branch_api_path_as("S", "p", "p", "T", "c", false));
    assert!(!at(None).contains("rev="));
}

#[test]
fn rebase_path_names_the_branched_package() {
    assert_eq!(
        rebase_api_path("home:u:branches:S", "libsigc++"),
        "/source/home:u:branches:S/libsigc%2B%2B?cmd=rebase"
    );
}

#[test]
fn following_the_devel_project_drops_ignoredevel() {
    let path = branch_api_path("S", "p", "T", "c", true);
    assert!(!path.contains("ignoredevel"), "{path}");
    assert!(branch_api_path("S", "p", "T", "c", false).contains("&ignoredevel=1&"));
}

#[test]
fn the_devel_project_comes_from_the_package_meta() {
    let meta = r#"<package name="foo" project="Base:System">
  <title/>
  <devel project="devel:languages:python" package="foo"/>
</package>"#;
    assert_eq!(
        parse_devel_project(meta).as_deref(),
        Some("devel:languages:python")
    );
    assert_eq!(
        parse_devel_project("<package name=\"foo\"><title/></package>"),
        None
    );
}

#[test]
fn branch_response_gives_the_origin_and_falls_back_to_the_request() {
    let xml = r#"<status code="ok"><summary>Ok</summary>
  <data name="targetproject">home:u:branches:X</data>
  <data name="targetpackage">foo</data>
  <data name="sourceproject">devel:languages:c_c++</data>
  <data name="sourcepackage">foo</data>
</status>"#;
    assert_eq!(
        parse_branch_response(xml, "X", "foo", "home:u:branches:X"),
        Branched {
            target_project: "home:u:branches:X".into(),
            target_package: "foo".into(),
            origin_project: "devel:languages:c_c++".into(),
        }
    );
    assert_eq!(
        parse_branch_response("<status code=\"ok\"/>", "X", "foo", "T").origin_project,
        "X"
    );
}

#[test]
fn branch_failures_are_told_apart() {
    use BranchFailure::*;
    let c = classify_branch_failure;
    assert_eq!(
        c("<status code=\"double_branch_package\">x</status>"),
        AlreadyThere
    );
    assert_eq!(c("branch target package already exists: a/b"), AlreadyThere);
    assert_eq!(c("project 'Base:System' is locked"), Locked);
    assert_eq!(c("<status code=\"project_locked\">x</status>"), Locked);
    // Neither word alone says what happened.
    assert_eq!(c("create_project: project already exists"), Other);
    assert_eq!(c("unlocked mirror lookup failed"), Other);
    assert_eq!(c("HTTP Error 404: Not Found"), NotFound);
    assert_eq!(c("unknown_package"), NotFound);
    assert_eq!(c("create_project_no_permission"), NoPermission);
    assert_eq!(c("HTTP Error 500"), Other);
}

#[test]
fn project_info_reads_kind_lock_and_repositories() {
    let info = parse_project_info(META);
    assert_eq!(info.kind, None);
    assert!(!info.locked);
    assert_eq!(info.repositories.len(), 2);
    assert_eq!(info.repositories[0].name, "openSUSE_Tumbleweed");
    assert_eq!(info.repositories[0].arches, ["x86_64", "aarch64"]);
    assert_eq!(
        info.repositories[1].paths,
        [
            ("Base:System".to_string(), "15.6".to_string()),
            ("openSUSE:Factory".to_string(), "snapshot".to_string())
        ]
    );

    let maint = "<project name=\"M\" kind=\"maintenance\"><lock><enable/></lock></project>";
    let info = parse_project_info(maint);
    assert_eq!(info.kind.as_deref(), Some("maintenance"));
    assert!(info.locked);
    assert!(!parse_project_info("<project name=\"M\"><lock><disable/></lock></project>").locked);
}

#[test]
fn project_info_unescapes_names_once() {
    let xml = "<project name=\"M\"><repository name=\"a&amp;lt;&#x41;&#66;&bogus;&quot;\">\
                   <arch>x&amp;86</arch></repository></project>";
    let info = parse_project_info(xml);
    assert_eq!(info.repositories[0].name, "a&lt;AB&bogus;\"");
    assert_eq!(info.repositories[0].arches, ["x&86"]);
}

#[test]
fn a_path_goes_after_the_last_existing_one() {
    let (xml, made) = add_paths_to_meta(META, &[add("15.6", "devel:tools", "15.6")]).unwrap();
    assert_eq!(made.len(), 1);
    assert!(xml.contains(
        "    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>\n    \
             <path project=\"devel:tools\" repository=\"15.6\"/>\n    <arch>x86_64</arch>"
    ));
    // The other repository is untouched.
    let info = parse_project_info(&xml);
    assert_eq!(info.repositories[0].paths.len(), 1);
    assert_eq!(info.repositories[1].paths.len(), 3);
}

#[test]
fn adding_a_path_twice_changes_nothing() {
    let adds = [
        add("15.6", "devel:tools", "15.6"),
        add("openSUSE_Tumbleweed", "devel:tools", "openSUSE_Tumbleweed"),
    ];
    let (once, made) = add_paths_to_meta(META, &adds).unwrap();
    assert_eq!(made.len(), 2);
    let (twice, made) = add_paths_to_meta(&once, &adds).unwrap();
    assert!(made.is_empty());
    assert_eq!(once, twice);
    // A path the meta already had is not added either.
    let (same, made) = add_paths_to_meta(META, &[add("15.6", "Base:System", "15.6")]).unwrap();
    assert!(made.is_empty());
    assert_eq!(same, META);
}

#[test]
fn several_repositories_are_edited_independently() {
    let adds = [
        add("openSUSE_Tumbleweed", "devel:a", "openSUSE_Tumbleweed"),
        add("15.6", "devel:a", "15.6"),
        add("15.6", "devel:b", "15.6"),
    ];
    let (xml, made) = add_paths_to_meta(META, &adds).unwrap();
    assert_eq!(made, adds);
    let info = parse_project_info(&xml);
    let tw: Vec<_> = info.repositories[0]
        .paths
        .iter()
        .map(|p| p.0.as_str())
        .collect();
    assert_eq!(tw, ["Base:System", "devel:a"]);
    let leap: Vec<_> = info.repositories[1]
        .paths
        .iter()
        .map(|p| p.0.as_str())
        .collect();
    assert_eq!(
        leap,
        ["Base:System", "openSUSE:Factory", "devel:a", "devel:b"]
    );
    // The arches survive.
    assert_eq!(info.repositories[0].arches, ["x86_64", "aarch64"]);
}

#[test]
fn a_repository_without_paths_gets_them_before_its_arches() {
    let meta = "<project name=\"p\">\n  <repository name=\"r\">\n    \
                    <arch>x86_64</arch>\n  </repository>\n</project>\n";
    let (xml, _) = add_paths_to_meta(meta, &[add("r", "devel:a", "x")]).unwrap();
    assert_eq!(
        xml,
        "<project name=\"p\">\n  <repository name=\"r\">\n    \
             <path project=\"devel:a\" repository=\"x\"/>\n    \
             <arch>x86_64</arch>\n  </repository>\n</project>\n"
    );
}

#[test]
fn a_repository_with_neither_paths_nor_arches_gets_a_path_in_its_body() {
    let meta = "<project name=\"p\">\n  <repository name=\"r\">\n  </repository>\n</project>\n";
    let (xml, _) = add_paths_to_meta(meta, &[add("r", "devel:a", "x")]).unwrap();
    assert!(xml.contains("<repository name=\"r\">\n"), "{xml}");
    assert_eq!(parse_project_info(&xml).repositories[0].paths.len(), 1);
}

#[test]
fn a_self_closing_repository_is_opened_up() {
    let meta = "<project name=\"p\">\n  <repository name=\"r\"/>\n</project>\n";
    let (xml, _) = add_paths_to_meta(meta, &[add("r", "devel:a", "x")]).unwrap();
    assert_eq!(
        xml,
        "<project name=\"p\">\n  <repository name=\"r\">\n    \
             <path project=\"devel:a\" repository=\"x\"/>\n  </repository>\n</project>\n"
    );
}

#[test]
fn a_repository_the_meta_lacks_is_an_error() {
    let err = add_paths_to_meta(META, &[add("nope", "a", "b")]).unwrap_err();
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn repository_names_that_only_share_a_prefix_are_not_confused() {
    let meta = "<project name=\"p\">\n  <repository name=\"15\">\n    \
                    <arch>x86_64</arch>\n  </repository>\n  <repository name=\"15.6\">\n    \
                    <arch>x86_64</arch>\n  </repository>\n</project>\n";
    let (xml, _) = add_paths_to_meta(meta, &[add("15.6", "devel:a", "x")]).unwrap();
    let info = parse_project_info(&xml);
    assert!(info.repositories[0].paths.is_empty());
    assert_eq!(info.repositories[1].paths.len(), 1);
}

#[test]
fn binary_search_ors_the_names_and_skips_quoted_ones() {
    assert_eq!(
        published_binary_search_path(&["foo", "foo-devel", "a'b"]),
        "/search/published/binary/id?match=%40name%3D%27foo%27%20or%20%40name%3D%27foo-devel%27"
    );
}

#[test]
fn names_with_quotes_or_backslashes_are_not_searchable() {
    assert!(is_searchable_binary_name("libfoo-devel"));
    assert!(is_searchable_binary_name("gtk+-3.0"));
    for bad in ["", "a'b", "a\"b", "a\\b"] {
        assert!(!is_searchable_binary_name(bad), "{bad:?}");
    }
    assert!(
        search_published_binaries(&["a'b"], Emit::quiet())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn published_binaries_are_read_from_the_search_result() {
    let xml = r#"<collection matches="2">
  <binary name="libfoo-devel" project="devel:languages:c_c++" package="foo" repository="openSUSE_Tumbleweed" version="1.2" release="3.1" arch="x86_64" filename="libfoo-devel-1.2-3.1.x86_64.rpm" type="rpm"/>
  <binary name="libfoo-devel" project="openSUSE:Factory" repository="snapshot" arch="x86_64"/>
  <binary name="broken" project="p"/>
</collection>"#;
    let bins = parse_published_binaries(xml);
    assert_eq!(bins.len(), 2);
    assert_eq!(bins[0].package.as_deref(), Some("foo"));
    assert_eq!(bins[0].version.as_deref(), Some("1.2"));
    assert_eq!(bins[1].package, None);
    assert_eq!(bins[1].repository, "snapshot");
}

fn sr() -> SubmitRequest {
    SubmitRequest {
        source_project: "home:u:branches:Base:System".into(),
        source_package: "foo".into(),
        target_project: "Base:System".into(),
        target_package: "foo".into(),
        description: "Update to 2 (was 1) & <more>".into(),
        source_rev: None,
    }
}

#[test]
fn submit_request_xml_pins_the_source_revision() {
    let mut r = sr();
    r.source_rev = Some("abc123".into());
    assert!(submit_request_xml(&r).contains(
        "<source project=\"home:u:branches:Base:System\" package=\"foo\" rev=\"abc123\"/>"
    ));
}

#[test]
fn directory_srcmd5_is_read_from_the_listing() {
    let xml = r#"<directory name="foo" rev="3" vrev="1" srcmd5="aaa111">
  <linkinfo project="o" package="foo" srcmd5="bbb222" baserev="bbb222"/>
  <entry name="foo.spec" md5="x" size="1" mtime="1"/>
</directory>"#;
    assert_eq!(parse_directory_srcmd5(xml).as_deref(), Some("aaa111"));
    assert_eq!(parse_directory_srcmd5("<nope/>"), None);
}

#[test]
fn a_moved_source_revision_refuses_the_request() {
    let mut r = sr();
    r.source_rev = Some("aaa".into());
    assert!(verify_source_rev(&r, || Ok("aaa".into())).is_ok());
    let err = verify_source_rev(&r, || Ok("bbb".into())).unwrap_err();
    assert!(err.to_string().contains("changed since"), "{err}");
    assert!(verify_source_rev(&r, || Err(Error::NotActionable("x".into()))).is_err());
    // An unpinned request is not compared (and the reader is not called).
    r.source_rev = None;
    assert!(verify_source_rev(&r, || panic!("read")).is_ok());
}

#[test]
fn submit_request_xml_names_both_ends_and_escapes_the_description() {
    assert_eq!(
        submit_request_xml(&sr()),
        "<request>\n  <action type=\"submit\">\n    \
             <source project=\"home:u:branches:Base:System\" package=\"foo\"/>\n    \
             <target project=\"Base:System\" package=\"foo\"/>\n  </action>\n  \
             <description>Update to 2 (was 1) &amp; &lt;more&gt;</description>\n</request>\n"
    );
}

#[test]
fn request_id_is_read_from_the_response() {
    assert_eq!(
        parse_request_id("<request id=\"1234567\" creator=\"u\">\n</request>").as_deref(),
        Some("1234567")
    );
    assert_eq!(parse_request_id("<status code=\"err\"/>"), None);
    assert_eq!(parse_request_id("<request id=\"x\">"), None);
    // Cut off before the closing `>`, and multi-byte text around.
    assert_eq!(parse_request_id("<request id=\"12\""), None);
    assert_eq!(xml_data_value("<data name=\"a\">é", "a"), None);
    assert_eq!(
        xml_data_value("é<data name=\"a\"> v </data>é", "a").as_deref(),
        Some("v")
    );
}

#[test]
fn open_requests_query_asks_for_open_submits_from_the_source() {
    assert_eq!(
        open_requests_path("home:u:branches:X", "foo"),
        "/request?view=collection&roles=source&types=submit&states=new,review\
             &project=home%3Au%3Abranches%3AX&package=foo"
    );
}

const REQUESTS: &str = r#"<collection matches="3">
  <request id="101" creator="u">
    <action type="submit">
      <source project="home:u:branches:Base:System" package="foo" rev="3"/>
      <target project="Base:System" package="foo"/>
    </action>
    <state name="review" who="u" when="2026-10-01T00:00:00"/>
    <description>Update foo</description>
  </request>
  <request id="102" creator="u">
    <action type="submit">
      <source project="home:u:branches:Base:System" package="foo"/>
      <target project="openSUSE:Factory" package="foo"/>
    </action>
    <action type="delete"><target project="x" package="y"/></action>
    <state name="new"/>
  </request>
  <request id="103"><action type="submit"><source project="a"/></action></request>
</collection>"#;

#[test]
fn open_requests_are_parsed_per_submit_action() {
    let reqs = parse_open_requests(REQUESTS);
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].id, "101");
    assert_eq!(reqs[0].state, "review");
    assert_eq!(reqs[0].target_project, "Base:System");
    assert_eq!(reqs[1].id, "102");
    assert_eq!(reqs[1].target_project, "openSUSE:Factory");
}

#[test]
fn a_request_to_another_target_is_not_a_duplicate() {
    let reqs = parse_open_requests(REQUESTS);
    let dup: Vec<_> = reqs
        .iter()
        .filter(|r| r.target_project == "Base:System" && r.source_package == "foo")
        .collect();
    assert_eq!(dup.len(), 1);
    assert_eq!(dup[0].id, "101");
    assert!(parse_open_requests("<collection matches=\"0\"/>").is_empty());
}
