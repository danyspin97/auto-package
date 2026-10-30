use super::*;

#[test]
fn extracts_project_and_repository_from_each_path() {
    let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
    <arch>x86_64</arch>
  </repository>
</project>
";
    let paths = parse_project_repo_paths(xml);
    assert_eq!(
        paths,
        vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
    );
}

#[test]
fn deduplicates_the_same_pair_across_repositories() {
    let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
  <repository name=\"openSUSE_Tumbleweed_i586\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
    let paths = parse_project_repo_paths(xml);
    assert_eq!(
        paths,
        vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
    );
}

#[test]
fn has_repository_true_when_one_is_present() {
    let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
    assert!(parse_has_repository(xml));
}

#[test]
fn has_repository_false_on_empty_project() {
    let xml = "\
<project name=\"home:dspinella:python\">
  <title>empty</title>
  <description/>
</project>
";
    assert!(!parse_has_repository(xml));
}

#[test]
fn has_repository_false_when_only_devel_project_present() {
    // A `<devel project="..."/>` element is not a build repository and
    // must not be mistaken for one.
    let xml = "\
<project name=\"home:dspinella:python\">
  <devel project=\"devel:languages:python\"/>
</project>
";
    assert!(!parse_has_repository(xml));
}

#[test]
fn a_devel_project_attribute_elsewhere_is_not_mistaken_for_a_path() {
    // `<devel project="..."/>` uses the same `project="..."` attribute
    // shape but is not a build path and has no `repository` attribute.
    let xml = "\
<project name=\"python-foo\">
  <devel project=\"devel:languages:python\"/>
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
    let paths = parse_project_repo_paths(xml);
    assert_eq!(
        paths,
        vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
    );
}

#[test]
fn build_paths_keep_the_repository_they_belong_to() {
    // The X11:Wayland shape: each repository names its own paths.
    let xml = "\
<project name=\"X11:Wayland\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
    <arch>x86_64</arch>
  </repository>
  <repository name=\"16.0\">
    <path project=\"openSUSE:Leap:16.0\" repository=\"standard\"/>
    <arch>x86_64</arch>
  </repository>
</project>
";
    assert_eq!(
        parse_build_paths(xml),
        vec![
            BuildPath {
                repository: "openSUSE_Tumbleweed".to_string(),
                project: "openSUSE:Factory".to_string(),
                path_repository: "snapshot".to_string(),
            },
            BuildPath {
                repository: "16.0".to_string(),
                project: "openSUSE:Leap:16.0".to_string(),
                path_repository: "standard".to_string(),
            },
        ]
    );
}
