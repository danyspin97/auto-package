use super::*;
use crate::{Cli, Commands};
use clap::Parser;

/// `update --project Base:System` with `extra` arguments.
fn args(extra: &[&str]) -> UpdateProjectArgs {
    let mut argv = vec!["auto-package", "update", "--project", "Base:System"];
    argv.extend_from_slice(extra);
    match Cli::try_parse_from(argv).unwrap().command {
        Commands::Update(a) => a,
        _ => panic!("not update"),
    }
}

/// Factory's meta for the package names `devel` as its devel project.
fn devel_is(devel: &str) -> Result<osc::FactoryPackage, String> {
    Ok(osc::FactoryPackage::Present {
        devel: Some(devel.into()),
    })
}

#[test]
fn factorys_devel_project_being_the_source_is_not_deprecated() {
    let state = factory_devel("Base:System", devel_is("Base:System"));
    assert_eq!(state, Some(FactoryDevel::Source));
    assert_eq!(skipped_devel(state, false), None);
}

#[test]
fn factorys_package_from_another_devel_project_is_skipped_without_the_flag() {
    let state = factory_devel("Base:System", devel_is("devel:languages"));
    assert_eq!(state, Some(FactoryDevel::Other("devel:languages".into())));
    assert_eq!(
        skipped_devel(state, false),
        Some("devel:languages".to_string())
    );
}

#[test]
fn factorys_package_from_another_devel_project_is_included_with_the_flag() {
    let state = factory_devel("Base:System", devel_is("devel:languages"));
    assert_eq!(skipped_devel(state, true), None);
}

#[test]
fn a_package_factory_does_not_have_is_not_deprecated() {
    // OBS answers 404: the package is new here, or Factory dropped it; the read
    // cannot say which, so it is not left out.
    let state = factory_devel("Base:System", Ok(osc::FactoryPackage::Absent));
    assert_eq!(state, None);
    assert_eq!(skipped_devel(state, false), None);
}

#[test]
fn a_failed_read_of_factory_is_not_deprecated() {
    let state = factory_devel("Base:System", Err("osc api timed out".into()));
    assert_eq!(state, None);
    assert_eq!(skipped_devel(state, false), None);
}

#[test]
fn a_factory_package_whose_meta_names_no_devel_project_is_not_deprecated() {
    let state = factory_devel(
        "Base:System",
        Ok(osc::FactoryPackage::Present { devel: None }),
    );
    assert_eq!(state, None);
}

#[test]
fn the_source_being_factory_itself_is_never_deprecated() {
    let state = factory_devel(FACTORY, devel_is("devel:languages"));
    assert_eq!(state, Some(FactoryDevel::Source));
    assert_eq!(skipped_devel(state, false), None);
}

#[test]
fn only_a_package_of_the_source_is_checked() {
    let a = args(&[]);
    assert!(applies("Base:System", &a));
    // A dependency from another project (`--update-deps`) is not.
    assert!(!applies("devel:other", &a));
}

#[test]
fn in_place_lint_and_maintenance_runs_are_not_checked() {
    assert!(!applies("Base:System", &args(&["--in-place"])));
    let mut lint = args(&[]);
    lint.improve = true;
    assert!(!applies("Base:System", &lint));
    let mut maintenance = args(&[]);
    maintenance.maintenance_source = true;
    assert!(!applies("Base:System", &maintenance));
}

#[test]
fn the_flag_with_any_form_of_maintenance_fails_the_argument_check() {
    let forms: [&[&str]; 4] = [
        &["--deprecated", "--maintenance"],
        &["--deprecated", "--maintenance", "SUSE:SLE-15:Update"],
        &["--deprecated", "--maintenance=all"],
        &["--deprecated", "--maintenance-list"],
    ];
    for extra in forms {
        let a = args(extra);
        assert_eq!(
            check_args(&a),
            Err("--deprecated is not supported with --maintenance yet".to_string()),
            "{extra:?}"
        );
    }
}

#[test]
fn the_flag_alone_is_accepted_and_refused_for_lint() {
    let a = args(&["--deprecated"]);
    assert!(a.deprecated);
    assert_eq!(check_args(&a), Ok(()));
    let mut lint = args(&["--deprecated"]);
    lint.improve = true;
    assert_eq!(
        check_args(&lint),
        Err("--deprecated is only for update".to_string())
    );
    assert_eq!(check_args(&args(&["--maintenance"])), Ok(()));
}

#[test]
fn the_reason_names_both_projects_and_the_flag() {
    let why = update::SkipReason::Deprecated {
        devel: "devel:languages".into(),
        source: "Base:System".into(),
    }
    .to_string();
    assert_eq!(
        why,
        "openSUSE:Factory lists devel:languages as the devel project of this package, not \
         Base:System: pass --deprecated to include it"
    );
}
