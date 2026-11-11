use super::package_flags::*;

/// The origin of a package with the flags the X11:Wayland meta has for noctalia.
const ORIGIN: &str = "<package name=\"noctalia\" project=\"X11:Wayland\">
  <title>Noctalia</title>
  <build>
    <disable repository=\"16.0\"/>
    <disable repository=\"16.1\"/>
    <disable arch=\"i586\" repository=\"openSUSE_Slowroll\"/>
  </build>
</package>
";

/// A branch made by OBS: the same package, no flags, with a devel project.
const BRANCH: &str = "<package name=\"noctalia\" project=\"home:jdoe:auto-package:X11:Wayland\">
  <title>Noctalia</title>
  <description>Shell &amp; bar</description>
  <person userid=\"jdoe\" role=\"maintainer\"/>
  <devel project=\"X11:Wayland\"/>
</package>
";

fn copied(origin: &str, branch: &str) -> (String, Vec<&'static str>) {
    match merge_build_flags(origin, branch).expect("merge") {
        FlagMerge::Changed { xml, kinds } => (xml, kinds),
        other => panic!("expected a change, got {other:?}"),
    }
}

#[test]
fn flags_the_origin_defines_are_copied_into_the_branch() {
    let (xml, kinds) = copied(ORIGIN, BRANCH);
    assert_eq!(kinds, vec!["build"]);
    assert!(xml.contains("<disable repository=\"16.0\"/>"));
    assert!(xml.contains("<disable arch=\"i586\" repository=\"openSUSE_Slowroll\"/>"));
    // The copied block is the origin's, in the origin's order.
    let build = xml.find("<build>").expect("a build block");
    assert!(xml.find("16.0").unwrap() > build);
    assert!(xml.find("16.1").unwrap() < xml.find("i586").unwrap());
}

#[test]
fn other_meta_elements_stay_byte_for_byte() {
    let (xml, _) = copied(ORIGIN, BRANCH);
    let expected = "<package name=\"noctalia\" project=\"home:jdoe:auto-package:X11:Wayland\">
  <title>Noctalia</title>
  <description>Shell &amp; bar</description>
  <person userid=\"jdoe\" role=\"maintainer\"/>
  <devel project=\"X11:Wayland\"/>
  <build>
    <disable repository=\"16.0\"/>
    <disable repository=\"16.1\"/>
    <disable arch=\"i586\" repository=\"openSUSE_Slowroll\"/>
  </build>
</package>
";
    assert_eq!(xml, expected);
}

#[test]
fn origin_without_flags_changes_nothing() {
    let origin = "<package name=\"p\" project=\"X11:Wayland\">\n  <title>P</title>\n</package>\n";
    assert_eq!(
        merge_build_flags(origin, BRANCH).unwrap(),
        FlagMerge::NoFlags
    );
}

#[test]
fn a_second_run_finds_the_flags_equal_and_writes_nothing() {
    let (once, _) = copied(ORIGIN, BRANCH);
    assert_eq!(merge_build_flags(ORIGIN, &once).unwrap(), FlagMerge::Equal);
}

#[test]
fn equal_flags_with_other_whitespace_and_attribute_order_count_as_equal() {
    let branch = "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <build>
      <disable  repository='16.0'/>
      <disable repository=\"16.1\" />
      <disable repository=\"openSUSE_Slowroll\" arch=\"i586\"/>
  </build>
</package>
";
    assert_eq!(merge_build_flags(ORIGIN, branch).unwrap(), FlagMerge::Equal);
}

#[test]
fn reuse_replaces_differing_flags_of_a_kind_the_origin_defines() {
    let branch = "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <build>
    <disable repository=\"16.0\"/>
  </build>
</package>
";
    let (xml, kinds) = copied(ORIGIN, branch);
    assert_eq!(kinds, vec!["build"]);
    assert_eq!(
        xml,
        "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <build>
    <disable repository=\"16.0\"/>
    <disable repository=\"16.1\"/>
    <disable arch=\"i586\" repository=\"openSUSE_Slowroll\"/>
  </build>
</package>
"
    );
}

#[test]
fn a_kind_the_origin_does_not_define_is_left_as_the_branch_has_it() {
    let branch = "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <useforbuild>
    <disable repository=\"Tumbleweed\"/>
  </useforbuild>
</package>
";
    let (xml, kinds) = copied(ORIGIN, branch);
    assert_eq!(kinds, vec!["build"]);
    assert!(
        xml.contains("<useforbuild>\n    <disable repository=\"Tumbleweed\"/>\n  </useforbuild>")
    );
}

#[test]
fn enable_and_disable_with_arch_and_repository_survive() {
    let origin = "<package name=\"nwg\" project=\"X11:Wayland\">
  <title>nwg</title>
  <build>
    <disable repository=\"openSUSE_Slowroll\"/>
    <enable arch=\"x86_64\" repository=\"openSUSE_Slowroll\"/>
  </build>
</package>
";
    let (xml, _) = copied(origin, BRANCH);
    let build = xml.find("<build>").expect("a build block");
    let disable = xml
        .find("<disable repository=\"openSUSE_Slowroll\"/>")
        .expect("disable");
    let enable = xml
        .find("<enable arch=\"x86_64\" repository=\"openSUSE_Slowroll\"/>")
        .expect("enable");
    assert!(build < disable && disable < enable);
    assert_eq!(merge_build_flags(origin, &xml).unwrap(), FlagMerge::Equal);
}

#[test]
fn several_kinds_are_added_in_flag_order() {
    let origin = "<package name=\"p\" project=\"X11:Wayland\">
  <publish>
    <disable/>
  </publish>
  <debuginfo>
    <disable/>
  </debuginfo>
</package>
";
    let (xml, kinds) = copied(origin, BRANCH);
    assert_eq!(kinds, vec!["publish", "debuginfo"]);
    assert!(xml.ends_with(
        "  <devel project=\"X11:Wayland\"/>\n  <publish>\n    <disable/>\n  </publish>\n  <debuginfo>\n    <disable/>\n  </debuginfo>\n</package>\n"
    ));
}

#[test]
fn a_branch_with_its_closing_tag_inline_gets_the_flags_before_it() {
    let branch = "<package name=\"p\" project=\"b\"><title>P</title></package>";
    let origin =
        "<package name=\"p\" project=\"o\"><build><disable repository=\"x\"/></build></package>";
    let (xml, _) = copied(origin, branch);
    assert_eq!(
        xml,
        "<package name=\"p\" project=\"b\"><title>P</title>\n  <build><disable repository=\"x\"/></build></package>"
    );
}

#[test]
fn malformed_origin_meta_is_an_error() {
    assert!(merge_build_flags("<package name=\"p\"><build></package>", BRANCH).is_err());
    assert!(merge_build_flags("<package name=\"p\"><build>", BRANCH).is_err());
    assert!(merge_build_flags("not xml at all <", BRANCH).is_err());
}

#[test]
fn malformed_branch_meta_is_an_error() {
    assert!(merge_build_flags(ORIGIN, "<package name=\"b\"><title></package>").is_err());
}

#[test]
fn a_document_that_is_not_a_package_meta_is_an_error() {
    let project = "<project name=\"b\"><title>B</title></project>";
    assert!(merge_build_flags(ORIGIN, project).is_err());
    assert!(merge_build_flags(project, BRANCH).is_err());
}

#[test]
fn two_blocks_of_one_kind_in_the_branch_collapse_into_the_origin_block() {
    let branch = "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <build>
    <disable repository=\"16.0\"/>
  </build>
  <build>
    <disable repository=\"stale\"/>
  </build>
</package>
";
    let (xml, kinds) = copied(ORIGIN, branch);
    assert_eq!(kinds, vec!["build"]);
    assert_eq!(
        xml,
        "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <build>
    <disable repository=\"16.0\"/>
    <disable repository=\"16.1\"/>
    <disable arch=\"i586\" repository=\"openSUSE_Slowroll\"/>
  </build>
</package>
"
    );
    assert_eq!(merge_build_flags(ORIGIN, &xml).unwrap(), FlagMerge::Equal);
}

#[test]
fn comments_and_cdata_inside_a_block_do_not_make_it_differ() {
    let origin = "<package name=\"p\" project=\"o\">
  <build>
    <!-- repositories the origin leaves out -->
    <disable repository=\"16.0\"/>
  </build>
</package>
";
    let branch = "<package name=\"p\" project=\"b\">
  <build>
    <disable repository=\"16.0\"/><![CDATA[ignored]]>
  </build>
</package>
";
    assert_eq!(merge_build_flags(origin, branch).unwrap(), FlagMerge::Equal);
}

#[test]
fn the_same_children_in_another_order_are_equal_and_not_rewritten() {
    let branch = "<package name=\"noctalia\" project=\"b\">
  <title>Noctalia</title>
  <build>
    <disable arch=\"i586\" repository=\"openSUSE_Slowroll\"/>
    <disable repository=\"16.1\"/>
    <disable repository=\"16.0\"/>
  </build>
</package>
";
    assert_eq!(merge_build_flags(ORIGIN, branch).unwrap(), FlagMerge::Equal);
}

#[test]
fn a_self_closing_branch_root_cannot_take_flags() {
    assert!(merge_build_flags(ORIGIN, "<package name=\"b\"/>").is_err());
}
