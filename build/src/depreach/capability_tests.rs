use super::*;
use crate::spec::{Bound, Requirement};

fn req(cap: &str, bound: Option<(&str, &str)>) -> Requirement {
    Requirement {
        dep: BuildRequire::Capability(cap.into()),
        bound: bound.map(|(op, v)| Bound {
            op: op.into(),
            version: v.into(),
        }),
    }
}

fn hit(project: &str, repo: &str, package: &str) -> Hit {
    Hit {
        project: project.into(),
        repository: repo.into(),
        package: Some(package.into()),
        version: Some("1".into()),
    }
}

fn cand(project: &str, repo: &str, name: &str, provides: &[&str]) -> Candidate {
    Candidate {
        name: name.into(),
        hit: hit(project, repo, name.trim_end_matches("-devel")),
        provides: Ok(provides.iter().map(|s| s.to_string()).collect()),
    }
}

fn reach(repo: &str, projects: &[&str], unread: usize) -> Reach {
    Reach {
        repo: repo.into(),
        projects: projects.iter().map(|p| p.to_string()).collect(),
        unread,
    }
}

/// Two repositories: `16.1` reaches the branch and Leap 16.1, `16.0` the
/// branch and Leap 16.0.
fn two_repos(unread: usize) -> Vec<Reach> {
    vec![
        reach("16.1", &["home:u:b", "openSUSE:Leap:16.1"], unread),
        reach("16.0", &["home:u:b", "openSUSE:Leap:16.0"], unread),
    ]
}

#[test]
fn needle_is_the_leading_word_of_the_capability() {
    assert_eq!(name_needle("pkgconfig(glib-2.0)").as_deref(), Some("glib"));
    assert_eq!(
        name_needle("pkgconfig(sdbus-c++)").as_deref(),
        Some("sdbus")
    );
    assert_eq!(name_needle("cmake(Qt6Core)"), None);
    assert_eq!(name_needle("%{python_module foo}"), None);
    assert_eq!(name_needle("pkgconfig(x)"), None);
}

#[test]
fn build_tools_come_from_devel_packages_and_modules_from_any() {
    assert!(may_provide("pkgconfig(glib-2.0)", "glib2-devel"));
    assert!(!may_provide("pkgconfig(glib-2.0)", "glib2-tools"));
    assert!(may_provide("perl(Foo::Bar)", "perl-Foo-Bar"));
}

#[test]
fn an_older_provider_in_the_paths_is_evidence() {
    let cands = [cand(
        "openSUSE:Leap:16.0",
        "standard",
        "pixman-devel",
        &["pkgconfig(pixman-1) = 0.43.0"],
    )];
    let state = judge_capability(
        &req("pkgconfig(pixman-1)", Some((">=", "0.46"))),
        "pkgconfig(pixman-1)",
        &two_repos(0),
        &cands,
    );
    let CapState::NoProvider {
        found,
        unknown,
        name,
    } = state
    else {
        panic!("{state:?}");
    };
    assert_eq!(name, "pixman");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].repo, "16.0");
    assert_eq!(found[0].rpm, "pixman-devel");
    assert_eq!(found[0].version, "0.43.0");
    assert_eq!(unknown.len(), 1, "{unknown:?}");
}

#[test]
fn a_provider_that_meets_the_bound_is_provided() {
    let cands = [cand(
        "openSUSE:Leap:16.1",
        "standard",
        "glib2-devel",
        &["pkgconfig(glib-2.0) = 2.80.0"],
    )];
    let state = judge_capability(
        &req("pkgconfig(glib-2.0)", Some((">=", "2.70"))),
        "pkgconfig(glib-2.0)",
        &[reach("16.1", &["openSUSE:Leap:16.1"], 0)],
        &cands,
    );
    assert!(matches!(state, CapState::Provided { .. }), "{state:?}");
}

#[test]
fn no_candidate_named_for_it_is_unchecked_not_missing() {
    for cap in ["pkgconfig(libudev)", "pkgconfig(egl)", "pkgconfig(x11)"] {
        let state = judge_capability(&req(cap, Some((">=", "1"))), cap, &two_repos(0), &[]);
        assert!(matches!(state, CapState::Unchecked(_)), "{cap}: {state:?}");
    }
}

#[test]
fn a_candidate_outside_the_reach_is_not_judged() {
    let cands = [cand(
        "devel:gnome",
        "16.1",
        "hyprutils-devel",
        &["pkgconfig(hyprutils) = 0.2.3"],
    )];
    let state = judge_capability(
        &req("pkgconfig(hyprutils)", Some((">=", "0.7.1"))),
        "pkgconfig(hyprutils)",
        &two_repos(0),
        &cands,
    );
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn a_factory_hit_in_standard_counts_for_a_snapshot_path() {
    // OBS publishes Factory's binaries under `standard` only; the path names
    // `snapshot`. Judging by the project, the hit is in the paths.
    let cands = [cand(
        "openSUSE:Factory",
        "standard",
        "pixman-devel",
        &["pkgconfig(pixman-1) = 0.43.0"],
    )];
    let tw = [reach(
        "openSUSE_Tumbleweed",
        &["home:u:b", "openSUSE:Factory"],
        0,
    )];
    let state = judge_capability(
        &req("pkgconfig(pixman-1)", Some((">=", "0.46"))),
        "pkgconfig(pixman-1)",
        &tw,
        &cands,
    );
    let CapState::NoProvider { found, .. } = state else {
        panic!("{state:?}");
    };
    assert_eq!(found[0].project, "openSUSE:Factory");
    assert_eq!(found[0].version, "0.43.0");
}

#[test]
fn an_unread_candidate_makes_an_older_one_unchecked() {
    let cands = [cand(
        "openSUSE:Leap:16.1",
        "standard",
        "wayland-devel",
        &["pkgconfig(wayland-client) = 1.20.0"],
    )];
    let state = judge_capability(
        &req("pkgconfig(wayland-client)", Some((">=", "1.23"))),
        "pkgconfig(wayland-client)",
        &[reach("16.1", &["openSUSE:Leap:16.1"], 3)],
        &cands,
    );
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn a_failed_read_is_unchecked_and_not_a_warning() {
    let cands = [Candidate {
        name: "foo-devel".into(),
        hit: hit("openSUSE:Leap:16.1", "standard", "foo"),
        provides: Err("HTTP Error 500".into()),
    }];
    let state = judge_capability(
        &req("pkgconfig(foo)", Some((">=", "1"))),
        "pkgconfig(foo)",
        &[reach("16.1", &["openSUSE:Leap:16.1"], 0)],
        &cands,
    );
    match state {
        CapState::Unchecked(why) => assert!(why.contains("16.1"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_provider_in_one_repository_and_older_in_the_other_warns_about_that_one() {
    let cands = [
        cand(
            "openSUSE:Leap:16.1",
            "standard",
            "hyprutils-devel",
            &["pkgconfig(hyprutils) = 0.7.2"],
        ),
        cand(
            "openSUSE:Leap:16.0",
            "standard",
            "hyprutils-devel",
            &["pkgconfig(hyprutils) = 0.6.0"],
        ),
    ];
    let state = judge_capability(
        &req("pkgconfig(hyprutils)", Some((">=", "0.7.1"))),
        "pkgconfig(hyprutils)",
        &two_repos(0),
        &cands,
    );
    let CapState::NoProvider { found, .. } = state else {
        panic!("{state:?}");
    };
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].repo, "16.0");
}

#[test]
fn an_unversioned_provide_cannot_be_judged_against_a_bound() {
    let cands = [cand(
        "openSUSE:Leap:16.1",
        "standard",
        "foo-devel",
        &["pkgconfig(foo)"],
    )];
    let state = judge_capability(
        &req("pkgconfig(foo)", Some((">=", "1"))),
        "pkgconfig(foo)",
        &[reach("16.1", &["openSUSE:Leap:16.1"], 0)],
        &cands,
    );
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn an_unbounded_requirement_is_met_by_any_provider() {
    let cands = [cand(
        "openSUSE:Leap:16.1",
        "standard",
        "foo-devel",
        &["pkgconfig(foo) = 0.1"],
    )];
    let state = judge_capability(
        &req("pkgconfig(foo)", None),
        "pkgconfig(foo)",
        &[reach("16.1", &["openSUSE:Leap:16.1"], 0)],
        &cands,
    );
    assert!(matches!(state, CapState::Provided { .. }), "{state:?}");
}

/// `provided_within` for `pkgconfig(foo)` with one bound, the provides given
/// as the texts OBS writes.
fn within(bound: (&str, &str), provides: &[&str]) -> Match {
    let provides: Vec<String> = provides.iter().map(|s| s.to_string()).collect();
    provided_within(
        &req("pkgconfig(foo)", Some(bound)),
        "pkgconfig(foo)",
        &provides,
    )
}

#[test]
fn a_bound_without_a_release_ignores_the_release_of_the_provide() {
    let have = ["pkgconfig(foo) = 1.2.3-4"];
    assert_eq!(within(("=", "1.2.3"), &have), Match::Satisfied);
    assert_eq!(within(("<=", "1.2.3"), &have), Match::Satisfied);
    assert_eq!(within((">=", "1.2.3"), &have), Match::Satisfied);
    assert_eq!(
        within(("<", "1.2.3"), &have),
        Match::Older("1.2.3-4".into())
    );
    assert_eq!(
        within((">", "1.2.3"), &have),
        Match::Older("1.2.3-4".into())
    );
}

#[test]
fn a_bound_with_a_release_compares_the_release_too() {
    let same = ["pkgconfig(foo) = 1.2.3-4"];
    let newer = ["pkgconfig(foo) = 1.2.3-5"];
    assert_eq!(within(("=", "1.2.3-4"), &same), Match::Satisfied);
    assert_eq!(
        within(("=", "1.2.3-4"), &newer),
        Match::Older("1.2.3-5".into())
    );
}

#[test]
fn an_epoch_counts_before_the_version() {
    let epoch = ["pkgconfig(foo) = 1:1.2.3-4"];
    assert_eq!(
        within(("=", "1.2.3"), &epoch),
        Match::Older("1:1.2.3-4".into())
    );
    assert_eq!(within((">=", "1:1.2.3"), &epoch), Match::Satisfied);
}
