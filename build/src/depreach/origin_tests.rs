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

/// A `%{python_module}` requirement, looked up by its flavored binary name:
/// the branch builds against `devel:languages:python:numeric`, which ships
/// `python313-numpy` 1.26 from `python-numpy`.
mod python_module {
    use super::*;
    use crate::spec::{Requirement, build_requirements};

    const NUMERIC: &str = "devel:languages:python:numeric";
    const SOURCE: &str = "devel:languages:python";

    fn origins_of(spec: &str) -> (Vec<Requirement>, Vec<DependencyOrigin>) {
        let mut obs = fixture();
        obs.metas.insert(
            OWN.into(),
            project(
                OWN,
                &[("openSUSE_Tumbleweed", NUMERIC, "openSUSE_Tumbleweed")],
            ),
        );
        obs.metas.insert(
            NUMERIC.into(),
            project(NUMERIC, &[("openSUSE_Tumbleweed", "", "")]),
        );
        for name in ["python313-numpy", "python313-ruamel.yaml"] {
            obs.published.push(PublishedBinary {
                name: name.into(),
                project: NUMERIC.into(),
                repository: "openSUSE_Tumbleweed".into(),
                arch: "x86_64".into(),
                package: Some(name.replace("python313-", "python-")),
                version: Some("1.26.0".into()),
            });
        }
        let r = resolver(obs);
        let reqs =
            expand_python_modules(&build_requirements(spec), &["python313".into()]).requirements;
        let deps: Vec<BuildRequire> = reqs.iter().map(|r| r.dep.clone()).collect();
        let mut verdicts = r.check(&deps).unwrap();
        verdicts.extend(
            r.check_requirements(&reqs, "python-app", SOURCE, &[])
                .unwrap(),
        );
        let origins = dependency_origins(&verdicts, &[], OWN, SOURCE, true);
        (reqs, origins)
    }

    fn need(spec: &str) -> DepNeed {
        let (reqs, origins) = origins_of(spec);
        assert_eq!(origins.len(), 1, "{origins:?}");
        let o = &origins[0];
        dependency_need(&reqs, &o.names, "1.26.0", &o.provided, "2.1.0")
    }

    #[test]
    fn a_bound_the_origin_misses_makes_its_package_an_origin_to_update() {
        let (_, origins) = origins_of("BuildRequires: %{python_module numpy >= 2.0}\n");
        assert_eq!(
            origins,
            vec![DependencyOrigin {
                project: NUMERIC.into(),
                package: "python-numpy".into(),
                names: vec!["python313-numpy".into()],
                provided: Vec::new(),
            }]
        );
        assert_eq!(
            need("BuildRequires: %{python_module numpy >= 2.0}\n"),
            DepNeed::Update("python313-numpy >= 2.0".into())
        );
    }

    #[test]
    fn a_bound_the_origin_meets_or_no_bound_keeps_the_origin() {
        assert_eq!(
            need("BuildRequires: %{python_module numpy >= 1.20}\n"),
            DepNeed::Origin
        );
        assert_eq!(
            need("BuildRequires: %{python_module numpy}\n"),
            DepNeed::Origin
        );
    }

    #[test]
    fn a_name_with_a_dot_is_looked_up_as_the_macro_spells_it() {
        assert_eq!(
            need("BuildRequires: %{python_module ruamel.yaml > 1.26.0}\n"),
            DepNeed::Update("python313-ruamel.yaml > 1.26.0".into())
        );
    }

    #[test]
    fn a_bound_from_pypi_reaches_a_module_the_spec_names_without_one() {
        let (_, origins) = origins_of("BuildRequires: %{python_module numpy}\n");
        let predicted = [PredictedRequirement {
            pypi_name: "numpy".into(),
            requirement: Requirement {
                dep: BuildRequire::Package("python3-numpy".into()),
                bound: Some(crate::spec::Bound {
                    op: ">=".into(),
                    version: "2.0".into(),
                }),
            },
        }];
        let got = predicted_need(
            &predicted,
            &origins[0].package,
            &origins[0].names,
            "1.26.0",
            "2.1.0",
        );
        assert_eq!(got, Some(DepNeed::Update("python3-numpy >= 2.0".into())));
    }

    #[test]
    fn without_flavors_a_module_stays_unchecked_and_is_no_origin() {
        let mut obs = fixture();
        obs.metas.insert(
            OWN.into(),
            project(
                OWN,
                &[("openSUSE_Tumbleweed", NUMERIC, "openSUSE_Tumbleweed")],
            ),
        );
        let r = resolver(obs);
        let reqs = expand_python_modules(
            &build_requirements("BuildRequires: %{python_module numpy >= 2}\n"),
            &[],
        )
        .requirements;
        let verdicts = r
            .check_requirements(&reqs, "python-app", SOURCE, &[])
            .unwrap();
        assert!(
            matches!(
                verdicts[..],
                [DepVerdict {
                    state: DepState::Capability(CapState::Unchecked(_)),
                    ..
                }]
            ),
            "{verdicts:?}"
        );
        assert!(dependency_origins(&verdicts, &[], OWN, SOURCE, true).is_empty());
    }
}
