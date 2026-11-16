use super::*;

const PLAIN: &str = r#"<sourceinfo package="fanficfare" rev="7" vrev="1" srcmd5="b3ed" verifymd5="b3ed">
    <filename>fanficfare.spec</filename>
  </sourceinfo>"#;

const LINK: &str = r#"<sourceinfo package="cookiecutter" rev="40" vrev="42" srcmd5="0ea5" lsrcmd5="c9b5" verifymd5="0b10">
    <filename>cookiecutter.spec</filename>
    <linked project="openSUSE:Factory" package="cookiecutter"/>
  </sourceinfo>"#;

const BROKEN: &str = r#"<sourceinfo package="python-urllib3_1" rev="44" srcmd5="f4d8">
    <error>openSUSE:Factory/python-urllib3_1: package 'python-urllib3_1' does not exist</error>
    <linked project="openSUSE:Factory" package="python-urllib3_1"/>
  </sourceinfo>"#;

const NO_SPEC: &str = r#"<sourceinfo package="32bitaggregates" rev="3" srcmd5="4c5b">
    <filename>_aggregate</filename>
  </sourceinfo>"#;

const SELF_CLOSING: &str = r#"<sourceinfo package="bare" rev="1" srcmd5="9999" />"#;

#[test]
fn a_plain_and_a_link_entry_give_their_expanded_srcmd5() {
    let xml = format!("<sourceinfolist>\n{PLAIN}\n{LINK}\n</sourceinfolist>");
    let got = parse_spec_revisions(&xml);
    assert_eq!(got.get("fanficfare").map(String::as_str), Some("b3ed"));
    assert_eq!(got.get("cookiecutter").map(String::as_str), Some("0ea5"));
    assert_eq!(got.len(), 2);
}

#[test]
fn a_broken_link_a_package_without_a_spec_and_an_empty_entry_are_left_out() {
    let xml = format!("<sourceinfolist>{BROKEN}{NO_SPEC}{SELF_CLOSING}</sourceinfolist>");
    assert!(parse_spec_revisions(&xml).is_empty());
}

#[test]
fn a_self_closing_entry_does_not_swallow_the_next_one() {
    let xml = format!("<sourceinfolist>{SELF_CLOSING}{PLAIN}</sourceinfolist>");
    let got = parse_spec_revisions(&xml);
    assert_eq!(got.len(), 1);
    assert!(got.contains_key("fanficfare"));
}

#[test]
fn the_link_revision_is_not_read_for_srcmd5() {
    let xml = r#"<sourceinfo package="x" srcmd5="" lsrcmd5="c9b5"><filename>x.spec</filename></sourceinfo>"#;
    assert!(parse_spec_revisions(xml).is_empty());
}

#[test]
fn an_unclosed_entry_is_read_to_the_end_without_a_panic() {
    let xml = r#"<sourceinfo package="x" srcmd5="1"><filename>x.spec</filename>"#;
    assert_eq!(
        parse_spec_revisions(xml).get("x").map(String::as_str),
        Some("1")
    );
}

#[test]
fn an_invalid_project_name_asks_nothing() {
    assert_eq!(project_spec_revisions("../etc"), None);
}

#[test]
fn a_listing_without_a_spec_file_has_no_revision() {
    assert_eq!(
        spec_revision_of_listing(
            "<directory name=\"x\" srcmd5=\"4c5b\"><entry name=\"_aggregate\"/></directory>"
        ),
        None
    );
    assert_eq!(
        spec_revision_of_listing(
            "<directory name=\"x\" srcmd5=\"\"><entry name=\"x.spec\"/></directory>"
        ),
        None
    );
}

#[test]
fn a_listing_with_a_spec_file_gives_its_expanded_revision() {
    let linked = "<directory name=\"x\" srcmd5=\"0ea5\"><linkinfo project=\"p\" package=\"x\" srcmd5=\"0b10\" lsrcmd5=\"c9b5\"/><entry name=\"x.spec\"/></directory>";
    assert_eq!(spec_revision_of_listing(linked).as_deref(), Some("0ea5"));
    let plain = "<directory name=\"x\" srcmd5=\"b3ed\"><entry name=\"x.spec\"/></directory>";
    assert_eq!(spec_revision_of_listing(plain).as_deref(), Some("b3ed"));
}
