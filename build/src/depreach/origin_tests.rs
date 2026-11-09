//! A capability an origin's provider does not meet is still a dependency
//! origin: the update its bound needs is in that project. Judged through the
//! resolver, against the fake OBS of `requirements_tests`.
use super::requirements_tests::{fixture, project, req, resolver, rpm};
use super::*;

const OWN: &str = "home:u:branches:X11:Wayland";

#[test]
fn a_provider_in_the_origin_that_misses_the_bound_makes_the_origin_a_dependency_origin() {
    let mut obs = fixture();
    // The branch builds against devel:libraries, which ships foo 2.0.
    obs.metas.insert(
        OWN.into(),
        project(OWN, &[("16.1", "devel:libraries", "16.1")]),
    );
    obs.metas.insert(
        "devel:libraries".into(),
        project("devel:libraries", &[("16.1", "", "")]),
    );
    obs.indexed
        .push(rpm("devel:libraries", "16.1", "foo-devel"));
    obs.provides.insert(
        ("devel:libraries".into(), "foo-devel".into()),
        Ok(vec!["pkgconfig(foo) = 2.0".into()]),
    );
    let r = resolver(obs);
    let reqs = [req("pkgconfig(foo)", Some((">=", "2.1")))];
    let verdicts = r
        .check_requirements(&reqs, "app", "Base:System", &[])
        .unwrap();
    assert_eq!(verdicts.len(), 1, "{verdicts:?}");
    assert!(
        matches!(
            verdicts[0].state,
            DepState::Capability(CapState::NoProvider { .. })
        ),
        "{verdicts:?}"
    );
    let origins = dependency_origins(&verdicts, &[], OWN, "Base:System", true);
    assert_eq!(
        origins,
        vec![DependencyOrigin {
            project: "devel:libraries".into(),
            package: "foo".into(),
            names: vec!["pkgconfig(foo) >= 2.1".into()],
            provided: vec![("pkgconfig(foo) >= 2.1".into(), "2.0".into())],
        }]
    );
    // --test-against-updates keeps the origins it had: a missed bound adds none.
    assert!(dependency_origins(&verdicts, &[], OWN, "Base:System", false).is_empty());
}

#[test]
fn a_provider_that_meets_the_bound_gives_the_origin_its_provide_version() {
    // The package is at 2.0 in devel:libraries, but its RPM provides 2.5: the
    // origin side of the bound is 2.5, so the origin is not updated for it.
    let mut obs = fixture();
    obs.metas.insert(
        OWN.into(),
        project(OWN, &[("16.1", "devel:libraries", "16.1")]),
    );
    obs.metas.insert(
        "devel:libraries".into(),
        project("devel:libraries", &[("16.1", "", "")]),
    );
    obs.indexed
        .push(rpm("devel:libraries", "16.1", "foo-devel"));
    obs.provides.insert(
        ("devel:libraries".into(), "foo-devel".into()),
        Ok(vec!["pkgconfig(foo) = 2.5".into()]),
    );
    let r = resolver(obs);
    let reqs = [req("pkgconfig(foo)", Some((">=", "2.1")))];
    let verdicts = r
        .check_requirements(&reqs, "app", "Base:System", &[])
        .unwrap();
    let origins = dependency_origins(&verdicts, &[], OWN, "Base:System", true);
    assert_eq!(
        origins,
        vec![DependencyOrigin {
            project: "devel:libraries".into(),
            package: "foo".into(),
            names: vec!["pkgconfig(foo) >= 2.1".into()],
            provided: vec![("pkgconfig(foo) >= 2.1".into(), "2.5".into())],
        }]
    );
}
