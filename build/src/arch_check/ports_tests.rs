use super::*;

const META: &str = "<project name=\"home:u:p\">\n  <repository name=\"standard\">\n    <arch>x86_64</arch>\n  </repository>\n</project>\n";

fn zsystems() -> Port {
    PORTS[0]
}

#[test]
fn the_ports_are_the_three_factory_ports_and_no_armv7l() {
    let archs: Vec<&str> = PORTS.iter().map(|p| p.arch).collect();
    assert_eq!(archs, ["s390x", "ppc64le", "riscv64"]);
    assert!(PORTS.iter().all(|p| p.project.starts_with("openSUSE:Factory:")));
    assert!(!PORTS.iter().any(|p| p.arch == "armv7l"));
}

#[test]
fn a_port_is_chosen_by_its_wanted_arches() {
    assert_eq!(ports_wanted(None).len(), 3);
    let s390x = vec!["s390x".to_string()];
    assert_eq!(ports_wanted(Some(&s390x)), vec![zsystems()]);
    let x86 = vec!["x86_64".to_string()];
    assert!(ports_wanted(Some(&x86)).is_empty());
    assert!(is_port_repo("openSUSE_Factory_RISCV"));
    assert!(!is_port_repo("standard"));
}

#[test]
fn the_repository_and_the_disable_are_added_exactly() {
    let (edited, changed) = add_port_to_meta(META, &zsystems()).unwrap();
    assert!(changed);
    let expected = "<project name=\"home:u:p\">\n  <repository name=\"standard\">\n    <arch>x86_64</arch>\n  </repository>\n  <repository name=\"openSUSE_Factory_zSystems\"><path project=\"openSUSE:Factory:zSystems\" repository=\"standard\"/><arch>s390x</arch></repository>\n  <build>\n    <disable repository=\"openSUSE_Factory_zSystems\"/>\n  </build>\n</project>\n";
    assert_eq!(edited, expected);
}

#[test]
fn a_second_run_changes_nothing() {
    let (once, _) = add_port_to_meta(META, &zsystems()).unwrap();
    let (twice, changed) = add_port_to_meta(&once, &zsystems()).unwrap();
    assert!(!changed);
    assert_eq!(once, twice);
}

#[test]
fn an_existing_build_block_keeps_its_flags_and_gets_the_disable() {
    let xml = META.replace(
        "</project>",
        "  <build>\n    <disable arch=\"armv7l\"/>\n  </build>\n</project>",
    );
    let (edited, changed) = add_port_to_meta(&xml, &zsystems()).unwrap();
    assert!(changed);
    assert!(edited.contains("<disable arch=\"armv7l\"/>"));
    assert!(edited.contains("    <disable repository=\"openSUSE_Factory_zSystems\"/>\n  </build>"));
    assert_eq!(edited.matches("<build>").count(), 1);
}

mod states {
    use super::*;
    use crate::osc::BuildStatus;

    #[test]
    fn a_port_result_is_classified_as_a_port_result() {
        assert_eq!(
            classify_port(&BuildStatus::Succeeded, None),
            ArchState::Succeeded
        );
        assert_eq!(
            classify_port(&BuildStatus::Excluded, None),
            ArchState::Ignored
        );
        assert_eq!(
            classify_port(&BuildStatus::Unresolvable, Some("nothing provides libwlroots-0_20 needed by x")),
            ArchState::NotTestable {
                detail: Some("nothing provides libwlroots-0_20".to_string())
            }
        );
        assert_eq!(
            classify_port(&BuildStatus::Unresolvable, None),
            ArchState::NotTestable { detail: None }
        );
        assert_eq!(
            classify_port(&BuildStatus::Broken, None),
            ArchState::NotTestable {
                detail: Some("broken".to_string())
            }
        );
        assert_eq!(
            classify_port(&BuildStatus::Building, None),
            ArchState::NotFinal("building".to_string())
        );
        assert!(matches!(
            classify_port(&BuildStatus::Failed, None),
            ArchState::Failed { .. }
        ));
    }

    #[test]
    fn a_port_failure_is_judged_against_factory() {
        let failed = || ArchState::Failed { detail: None };
        assert_eq!(
            judge_port(failed(), Some(&failed())),
            (ArchState::Failed { detail: None }, true),
            "Factory fails too: not the update's"
        );
        assert_eq!(
            judge_port(failed(), Some(&ArchState::Succeeded)),
            (ArchState::Failed { detail: None }, false),
            "Factory builds it: the update's"
        );
        assert_eq!(
            judge_port(failed(), None),
            (ArchState::NoBaseline { detail: None }, false),
            "no Factory result: it blocks, and says so"
        );
        assert_eq!(
            judge_port(failed(), Some(&ArchState::Pending("building".into()))),
            (ArchState::NotFinal("baseline building".into()), false)
        );
        let not_testable = ArchState::NotTestable { detail: None };
        assert_eq!(judge_port(not_testable.clone(), None), (not_testable, false));
    }

    #[test]
    fn only_a_failing_port_result_blocks_and_it_names_the_baseline() {
        assert!(ArchState::NoBaseline { detail: None }.is_failure());
        assert!(!ArchState::NotTestable { detail: None }.is_failure());
        assert!(!ArchState::NotFinal("building".into()).is_failure());
        assert_eq!(
            ArchState::NoBaseline { detail: None }.label(),
            "failed (no Factory baseline)"
        );
        assert_eq!(ArchState::NotFinal("building".into()).label(), "not final (building)");
    }
}
