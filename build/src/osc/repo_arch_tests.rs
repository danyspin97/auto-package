use super::*;

const META: &str = r#"<project name="home:u:p">
  <repository name="openSUSE_Tumbleweed">
    <path project="openSUSE:Factory" repository="snapshot"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
</project>"#;

#[test]
fn a_repository_and_arch_the_project_builds_are_accepted() {
    let info = parse_project_info(META);
    assert!(check_repo_arch(&info, "home:u:p", "openSUSE_Tumbleweed", "aarch64").is_ok());
}

#[test]
fn a_repository_the_project_calls_by_another_name_is_found() {
    let factory = META.replace("openSUSE_Tumbleweed", "openSUSE_Factory");
    let info = parse_project_info(&factory);
    assert_eq!(
        pick_repository(&info, "openSUSE_Tumbleweed", "x86_64").as_deref(),
        Ok("openSUSE_Factory")
    );
    let exact = parse_project_info(META);
    assert_eq!(
        pick_repository(&exact, "openSUSE_Tumbleweed", "x86_64").as_deref(),
        Ok("openSUSE_Tumbleweed")
    );
}

#[test]
fn with_no_request_tumbleweed_then_factory_then_the_first_is_chosen() {
    let repos = |names: &[&str]| {
        let body: String = names
            .iter()
            .map(|n| format!(r#"<repository name="{n}"><arch>x86_64</arch></repository>"#))
            .collect();
        parse_project_info(&format!(r#"<project name="p">{body}</project>"#))
    };
    let both = repos(&["openSUSE_Factory", "openSUSE_Tumbleweed", "Leap_16"]);
    let (chosen, others) = default_repository(&both, "x86_64").unwrap();
    assert_eq!(chosen, "openSUSE_Tumbleweed");
    assert_eq!(others, ["Leap_16", "openSUSE_Factory"]);
    let (chosen, _) =
        default_repository(&repos(&["Leap_16", "openSUSE_Factory"]), "x86_64").unwrap();
    assert_eq!(chosen, "openSUSE_Factory");
    let (chosen, others) = default_repository(&repos(&["b", "a"]), "x86_64").unwrap();
    assert_eq!((chosen.as_str(), others.len()), ("a", 1));
    assert_eq!(default_repository(&repos(&["a"]), "aarch64"), None);
}

#[test]
fn the_only_repository_for_an_arch_stands_in_and_several_do_not() {
    let one = parse_project_info(&META.replace("openSUSE_Tumbleweed", "Leap_16"));
    assert_eq!(
        pick_repository(&one, "openSUSE_Tumbleweed", "x86_64").as_deref(),
        Ok("Leap_16")
    );
    let two = parse_project_info(
        r#"<project name="p"><repository name="A"><arch>x86_64</arch></repository>
               <repository name="B"><arch>x86_64</arch></repository></project>"#,
    );
    let e = pick_repository(&two, "openSUSE_Tumbleweed", "x86_64").unwrap_err();
    assert!(e.contains("A, B"), "{e}");
    assert!(pick_repository(&one, "openSUSE_Tumbleweed", "i586").is_err());
}

#[test]
fn a_misspelt_repository_or_missing_arch_names_what_exists() {
    let info = parse_project_info(META);
    let e = check_repo_arch(&info, "home:u:p", "Tumbleweed", "x86_64").unwrap_err();
    assert!(e.contains("openSUSE_Tumbleweed"), "{e}");
    let e = check_repo_arch(&info, "home:u:p", "openSUSE_Tumbleweed", "i586").unwrap_err();
    assert!(e.contains("x86_64, aarch64"), "{e}");
}

#[test]
fn a_meta_without_repositories_is_not_judged_here() {
    let info = parse_project_info("<project name=\"p\"/>");
    assert!(check_repo_arch(&info, "p", "r", "a").is_ok());
}
