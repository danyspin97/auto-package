use super::same_project::*;
use super::*;
use ap_build::depreach::{
    CapState, DepState, DepVerdict, Found as CapFound, Hit, PredictedRequirement, Prediction,
};
use ap_build::spec::build_requirements;

const SOURCE: &str = "devel:languages:python";
const BRANCH: &str = "home:u:branches:devel:languages:python";

fn item(pkg: &str, old: &str, new: &str) -> Item {
    Item {
        pkg: pkg.into(),
        origin: SOURCE.into(),
        ecosystem: ap_build::update::Ecosystem::GitRepo {
            url: format!("https://github.com/a/{pkg}"),
        },
        old_version: old.into(),
        new_version: new.into(),
        upstream_tag: None,
        dependency_of: None,
        requires: Vec::new(),
        findings: Vec::new(),
    }
}

/// An index with `found` (`(pkg, old, new)`), finished or not.
fn index(found: &[(&str, &str, &str)], finished: bool) -> FoundIndex {
    let mut i = FoundIndex::default();
    for (pkg, old, new) in found {
        i.insert(&item(pkg, old, new));
    }
    if finished {
        i.finish();
    }
    i
}

/// `libb` of the source, required as `libb-devel`, at `origin`.
fn libb(origin: &str) -> SameProjectProvider {
    SameProjectProvider {
        package: "libb".into(),
        names: vec!["libb-devel".into()],
        provided: Vec::new(),
        origin_version: Some(origin.into()),
    }
}

/// What liba, whose spec is `spec`, needs of `providers`.
fn need_with(spec: &str, providers: &[SameProjectProvider], idx: &FoundIndex) -> Vec<SameNeed> {
    let reqs = build_requirements(spec);
    same_project_need("liba", &reqs, &Prediction::Unknown, providers, idx)
        .into_iter()
        .map(|n| n.need)
        .collect()
}

/// What liba needs of libb 1.0 when the sweep found 2.0, by `spec`.
fn need(spec: &str) -> SameNeed {
    let idx = index(&[("libb", "1.0", "2.0")], false);
    need_with(spec, &[libb("1.0-3.1")], &idx).remove(0)
}

fn needs(bound: &str) -> SameNeed {
    SameNeed::Needs {
        new_version: "2.0".into(),
        bound: bound.into(),
    }
}

#[test]
fn the_index_folds_spellings_keeps_the_first_and_knows_the_job_key() {
    let mut i = index(&[("python-Foo_bar", "1", "2")], false);
    i.insert(&item("python-foo-bar", "1", "3"));
    let f = i.get("python.foo.bar").expect("found");
    assert_eq!(
        (f.new_version.as_str(), f.pkg.as_str()),
        ("2", "python-Foo_bar")
    );
    assert_eq!(f.key, visit_key(&item("python-Foo_bar", "1", "2")));
    assert!(!i.sweep_finished());
    i.finish();
    assert!(i.sweep_finished());
    assert!(i.get("other").is_none());
}

#[test]
fn a_bound_the_found_update_meets_and_the_source_misses_is_needed() {
    let spec = "BuildRequires: libb-devel >= 2.0\n";
    assert_eq!(need(spec), needs("libb-devel >= 2.0"));
    assert_eq!(
        need("BuildRequires: libb-devel > 1.0\n"),
        needs("libb-devel > 1.0")
    );
    assert_eq!(
        need("BuildRequires: libb-devel = 2.0\n"),
        needs("libb-devel = 2.0")
    );
}

#[test]
fn a_bound_the_source_meets_or_no_bound_needs_nothing() {
    assert_eq!(
        need("BuildRequires: libb-devel >= 1.0\n"),
        SameNeed::NotNeeded
    );
    assert_eq!(need("BuildRequires: libb-devel\n"), SameNeed::NotNeeded);
    // An upper bound the source meets: updating would not be asked for.
    assert_eq!(
        need("BuildRequires: libb-devel < 2.0\n"),
        SameNeed::NotNeeded
    );
}

#[test]
fn a_bound_the_update_misses_too_is_unmet() {
    assert_eq!(
        need("BuildRequires: libb-devel >= 3.0\n"),
        SameNeed::Unmet {
            new_version: "2.0".into(),
            bound: "libb-devel >= 3.0".into()
        }
    );
    assert_eq!(
        need("BuildRequires: libb-devel < 1.0\n"),
        SameNeed::Unmet {
            new_version: "2.0".into(),
            bound: "libb-devel < 1.0".into()
        }
    );
}

#[test]
fn a_bound_that_cannot_be_compared_is_unknown() {
    assert_eq!(
        need("BuildRequires: libb-devel >= %{min_b}\n"),
        SameNeed::Unknown {
            new_version: "2.0".into(),
            bound: "libb-devel >= %{min_b}".into()
        }
    );
}

#[test]
fn epochs_compare_first_and_a_missing_one_is_zero() {
    // 2.0 has epoch 0, which no epoch-1 bound accepts.
    assert!(matches!(
        need("BuildRequires: libb-devel >= 1:1.5\n"),
        SameNeed::Unmet { .. }
    ));
    // An explicit epoch 0 is a missing one.
    assert_eq!(
        need("BuildRequires: libb-devel >= 0:2.0\n"),
        needs("libb-devel >= 0:2.0")
    );
    // A found update with the epoch meets it, the source without it does not.
    let idx = index(&[("libb", "1.0", "1:2.0")], false);
    let got = need_with("BuildRequires: libb-devel >= 1:1.5\n", &[libb("1.0")], &idx);
    assert_eq!(
        got,
        vec![SameNeed::Needs {
            new_version: "1:2.0".into(),
            bound: "libb-devel >= 1:1.5".into()
        }]
    );
}

#[test]
fn a_package_not_found_yet_is_waited_for_only_when_the_source_misses() {
    let idx = index(&[], false);
    let missed = need_with(
        "BuildRequires: libb-devel >= 2.0\n",
        &[libb("1.0-3.1")],
        &idx,
    );
    assert_eq!(
        missed,
        vec![SameNeed::NotFoundYet {
            bound: "libb-devel >= 2.0".into()
        }]
    );
    let met = need_with(
        "BuildRequires: libb-devel >= 1.0\n",
        &[libb("1.0-3.1")],
        &idx,
    );
    assert_eq!(met, vec![SameNeed::NotNeeded]);
    // A bound that cannot be compared may need the update: it waits too.
    let unknown = need_with("BuildRequires: libb-devel >= %{v}\n", &[libb("1.0")], &idx);
    assert!(matches!(unknown[0], SameNeed::NotFoundYet { .. }));
}

#[test]
fn a_package_the_finished_sweep_did_not_find_has_no_update() {
    let idx = index(&[("other", "1", "2")], true);
    let got = need_with("BuildRequires: libb-devel >= 2.0\n", &[libb("1.0")], &idx);
    assert_eq!(
        got,
        vec![SameNeed::NoUpdate {
            bound: "libb-devel >= 2.0".into()
        }]
    );
}

#[test]
fn a_capability_is_compared_with_its_provide() {
    let provider = SameProjectProvider {
        package: "libb".into(),
        names: vec!["pkgconfig(b) >= 2.0".into()],
        provided: vec![("pkgconfig(b) >= 2.0".into(), "1.0".into())],
        origin_version: None,
    };
    let spec = "BuildRequires: pkgconfig(b) >= 2.0\n";
    let found = index(&[("libb", "1.0", "2.0")], false);
    assert_eq!(
        need_with(spec, std::slice::from_ref(&provider), &found),
        vec![needs("pkgconfig(b) >= 2.0")]
    );
    let not_yet = index(&[], false);
    assert!(matches!(
        need_with(spec, &[provider], &not_yet)[0],
        SameNeed::NotFoundYet { .. }
    ));
}

#[test]
fn the_prediction_counts_and_says_where_it_comes_from() {
    let provider = SameProjectProvider {
        package: "python-foo".into(),
        names: vec!["python313-foo".into()],
        provided: Vec::new(),
        origin_version: Some("1.0".into()),
    };
    let prediction = Prediction::Requirements {
        version: "5.0".into(),
        requirements: vec![PredictedRequirement {
            pypi_name: "foo".into(),
            requirement: build_requirements("BuildRequires: python3-foo >= 2.0\n").remove(0),
        }],
    };
    let reqs = build_requirements("BuildRequires: python313-foo\n");
    let idx = index(&[("python-foo", "1.0", "2.1")], false);
    let got = same_project_need("python-bar", &reqs, &prediction, &[provider], &idx);
    assert_eq!(
        got[0].need,
        SameNeed::Needs {
            new_version: "2.1".into(),
            bound: "python3-foo >= 2.0 (PyPI's Requires-Dist of python-bar 5.0)".into()
        }
    );
}

#[test]
fn the_package_itself_is_never_its_own_dependency() {
    let idx = index(&[("liba", "1.0", "2.0")], false);
    let own = SameProjectProvider {
        package: "liba".into(),
        names: vec!["liba-devel".into()],
        provided: Vec::new(),
        origin_version: Some("1.0".into()),
    };
    assert!(need_with("BuildRequires: liba-devel >= 2.0\n", &[own], &idx).is_empty());
}

fn hit(project: &str, package: &str, version: &str) -> Hit {
    Hit {
        project: project.into(),
        repository: "standard".into(),
        package: Some(package.into()),
        version: Some(version.into()),
    }
}

fn reachable(name: &str, h: Hit) -> DepVerdict {
    DepVerdict {
        name: name.into(),
        state: DepState::Reachable { provider: h },
    }
}

#[test]
fn providers_are_the_source_packages_merged_by_package() {
    let verdicts = vec![
        reachable("libb-devel", hit(SOURCE, "libb", "1.0-2.1")),
        reachable("libb-tools", hit(SOURCE, "libb", "1.0-2.1")),
        reachable("libz-devel", hit("devel:libraries", "libz", "1.3")),
        DepVerdict {
            name: "pkgconfig(b) >= 2.0".into(),
            state: DepState::Capability(CapState::NoProvider {
                found: vec![CapFound {
                    repo: "standard".into(),
                    project: SOURCE.into(),
                    rpm: "libb-devel".into(),
                    version: "1.0".into(),
                    package: Some("libb".into()),
                }],
                unknown: Vec::new(),
                name: "pkgconfig(b)".into(),
            }),
        },
        DepVerdict {
            name: "pkgconfig(c)".into(),
            state: DepState::Capability(CapState::Provided {
                provider: hit(SOURCE, "libc", "4.2"),
            }),
        },
    ];
    let got = same_project_providers(&verdicts, SOURCE, BRANCH, &index(&[], false));
    assert_eq!(
        got,
        vec![
            SameProjectProvider {
                package: "libb".into(),
                names: vec![
                    "libb-devel".into(),
                    "libb-tools".into(),
                    "pkgconfig(b) >= 2.0".into()
                ],
                provided: vec![("pkgconfig(b) >= 2.0".into(), "1.0".into())],
                origin_version: Some("1.0-2.1".into()),
            },
            SameProjectProvider {
                package: "libc".into(),
                names: vec!["pkgconfig(c)".into()],
                provided: vec![("pkgconfig(c)".into(), "4.2".into())],
                origin_version: None,
            },
        ]
    );
}

#[test]
fn a_branch_hit_counts_only_for_a_package_the_sweep_found() {
    let verdicts = vec![
        reachable("libb-devel", hit(BRANCH, "libb", "2.0")),
        reachable("libz-devel", hit(BRANCH, "libz", "1.3")),
    ];
    let idx = index(&[("libb", "1.0", "2.0")], false);
    let got = same_project_providers(&verdicts, SOURCE, BRANCH, &idx);
    let names: Vec<&str> = got.iter().map(|p| p.package.as_str()).collect();
    assert_eq!(names, ["libb"]);
}

#[test]
fn a_capability_without_a_package_is_skipped() {
    let verdicts = vec![
        DepVerdict {
            name: "%{weird}".into(),
            state: DepState::Unchecked,
        },
        reachable(
            "libq",
            Hit {
                package: None,
                ..hit(SOURCE, "x", "1")
            },
        ),
    ];
    assert!(same_project_providers(&verdicts, SOURCE, BRANCH, &index(&[], false)).is_empty());
}

fn queued(pkgs: &[&str]) -> HashSet<String> {
    pkgs.iter().map(|p| visit_key(&item(p, "", ""))).collect()
}

fn n(dep: &str, need: SameNeed) -> NeedSameProject {
    NeedSameProject {
        dep: dep.into(),
        need,
    }
}

#[test]
fn a_queued_update_it_needs_is_waited_for_and_named() {
    let idx = index(&[("libb", "1.0", "2.0"), ("libc", "3", "4")], false);
    let order = same_project_after(
        "liba",
        &[
            n("libb", needs("libb-devel >= 2.0")),
            n("libc", SameNeed::NotNeeded),
        ],
        &idx,
        &queued(&["libb", "libc"]),
    );
    assert_eq!(
        order.waits,
        vec![
            WaitFor {
                dep: "libb".into(),
                key: format!("{SOURCE}/libb"),
                required: Some("libb-devel >= 2.0".into()),
                origin_version: "1.0".into(),
            },
            // Branched in this run, its branch hides the source's binaries.
            WaitFor {
                dep: "libc".into(),
                key: format!("{SOURCE}/libc"),
                required: None,
                origin_version: "3".into(),
            },
        ]
    );
    assert!(order.notes.is_empty());
}

#[test]
fn an_update_not_queued_is_not_waited_for_and_said() {
    let idx = index(&[("libb", "1.0", "2.0")], false);
    let order = same_project_after(
        "liba",
        &[n("libb", needs("libb-devel >= 2.0"))],
        &idx,
        &queued(&[]),
    );
    assert!(order.waits.is_empty());
    assert_eq!(
        order.notes,
        [
            "liba needs libb-devel >= 2.0, but this run does not build the update of libb: liba \
          builds against libb as the source has it"
        ]
    );
}

#[test]
fn unknown_waits_without_naming_and_unmet_or_no_update_is_said() {
    let idx = index(&[("libb", "1.0", "2.0")], true);
    let unknown = SameNeed::Unknown {
        new_version: "2.0".into(),
        bound: "libb-devel >= %{v}".into(),
    };
    let order = same_project_after("liba", &[n("libb", unknown)], &idx, &queued(&["libb"]));
    assert_eq!(order.waits.len(), 1);
    assert_eq!(order.waits[0].required, None);
    assert!(order.notes.is_empty());

    let unmet = SameNeed::Unmet {
        new_version: "2.0".into(),
        bound: "libb-devel >= 3".into(),
    };
    let none = SameNeed::NoUpdate {
        bound: "libd >= 9".into(),
    };
    let order = same_project_after(
        "liba",
        &[n("libb", unmet), n("libd", none)],
        &idx,
        &queued(&["libb"]),
    );
    assert_eq!(order.waits.len(), 1);
    assert_eq!(order.notes.len(), 2);
    assert!(order.notes[0].contains("neither libb as the source has it nor 2.0"));
    assert!(order.notes[1].contains("found no update of libd"));
}

#[test]
fn a_package_not_found_yet_adds_no_wait() {
    let idx = index(&[], false);
    let order = same_project_after(
        "liba",
        &[n("libb", SameNeed::NotFoundYet { bound: "x".into() })],
        &idx,
        &queued(&["libb"]),
    );
    assert_eq!(order, SameOrder::default());
}

#[test]
fn the_prediction_is_read_only_when_it_can_change_the_judgment() {
    let unbounded = build_requirements("BuildRequires: libb-devel\n");
    let bounded = build_requirements("BuildRequires: libb-devel >= 2.0\n");
    let not_found = index(&[], false);
    let found = index(&[("libb", "1.0", "2.0")], false);
    let providers = [libb("1.0-3.1")];
    // A bound, or an update the sweep found, can change it; neither does not.
    assert!(!needs_prediction(
        "liba", &unbounded, &providers, &not_found
    ));
    assert!(needs_prediction("liba", &bounded, &providers, &not_found));
    assert!(needs_prediction("liba", &unbounded, &providers, &found));
    // The package is never its own dependency.
    let own = [SameProjectProvider {
        package: "liba".into(),
        ..libb("1.0")
    }];
    assert!(!needs_prediction("liba", &bounded, &own, &found));
}
