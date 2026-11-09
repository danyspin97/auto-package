use super::*;
use ap_build::depreach::Prediction;
use ap_build::spec::build_requirements;
use clap::Parser;
use std::sync::Mutex;

#[derive(Parser)]
struct Wrapper {
    #[command(flatten)]
    args: UpdateProjectArgs,
}

fn parse(extra: &[&str]) -> Result<UpdateProjectArgs, clap::Error> {
    let mut argv = vec!["x", "Base:System"];
    argv.extend_from_slice(extra);
    Wrapper::try_parse_from(argv).map(|w| w.args)
}

const BRANCH: &str = "home:u:branches:Base:System";

/// libbar 1.0 in devel:libraries, with 2.0 released.
fn libbar() -> Item {
    Item {
        pkg: "libbar".into(),
        origin: "devel:libraries".into(),
        ecosystem: ap_build::update::Ecosystem::GitRepo {
            url: "https://github.com/a/libbar".into(),
        },
        old_version: "1.0".into(),
        new_version: "2.0".into(),
        upstream_tag: None,
        held_version: None,
        dependency_of: Some("libfoo".into()),
        requires: Vec::new(),
        findings: Vec::new(),
    }
}

fn libfoo() -> Item {
    let mut i = libbar();
    (i.pkg, i.origin, i.dependency_of) = ("libfoo".into(), "Base:System".into(), None);
    i
}

fn choose(test_against_updates: bool, spec: &str) -> DepChoice {
    choose_with(test_against_updates, spec, Vec::new())
}

/// `choose`, with what libbar's origin provides for the capabilities.
fn choose_with(
    test_against_updates: bool,
    spec: &str,
    provided: Vec<(String, String)>,
) -> DepChoice {
    let origin = depreach::DependencyOrigin {
        project: "devel:libraries".into(),
        package: "libbar".into(),
        names: vec!["libbar-devel".to_string()],
        provided,
        epoch: None,
    };
    let reqs = build_requirements(spec);
    choose_dependency(
        test_against_updates,
        &reqs,
        &Prediction::Unknown,
        &origin,
        &libbar(),
        "libfoo",
        BRANCH,
    )
}

/// python-bar 1.0 in devel:languages:python, with 2.0 released, which the
/// spec of python-foo requires as `python3-bar`.
fn python_bar() -> (Item, depreach::DependencyOrigin) {
    let mut bar = libbar();
    bar.pkg = "python-bar".into();
    bar.origin = "devel:languages:python".into();
    let origin = depreach::DependencyOrigin {
        project: bar.origin.clone(),
        package: bar.pkg.clone(),
        names: vec!["python3-bar".to_string()],
        provided: Vec::new(),
        epoch: None,
    };
    (bar, origin)
}

/// What becomes of python-bar for python-foo, whose spec is `spec` and whose
/// release 3.0 has PyPI's `requires_dist` `lines` (`None`: no prediction).
fn choose_predicted(spec: &str, lines: Option<&[&str]>) -> DepChoice {
    let (bar, origin) = python_bar();
    let prediction = match lines {
        Some(lines) => {
            let json = serde_json::json!({
                "info": { "version": "3.0", "requires_dist": lines },
            })
            .to_string();
            let pypi = ap_build::update::Ecosystem::Pypi {
                pypi_name: "foo".into(),
            };
            depreach::predicted_requirements(&pypi, "3.0", |_, _| Ok(Some(json)))
        }
        None => Prediction::Failed("no metadata".into()),
    };
    let reqs = build_requirements(spec);
    choose_dependency(
        false,
        &reqs,
        &prediction,
        &origin,
        &bar,
        "python-foo",
        BRANCH,
    )
}

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl Reporter for Lines {
    fn report(&self, event: &Event) {
        let line = match event {
            Event::Progress { message, .. } => message.clone(),
            Event::Warning { message, .. } => format!("warning: {message}"),
            _ => return,
        };
        self.0.lock().unwrap().push(line);
    }
}

#[test]
fn test_against_updates_needs_update_deps_and_is_off_by_default() {
    let a = parse(&["--update-deps"]).unwrap();
    assert!(a.update_deps && !a.test_against_updates);
    let a = parse(&["--update-deps", "--test-against-updates"]).unwrap();
    assert!(a.test_against_updates);
    assert!(parse(&["--test-against-updates"]).is_err());
}

#[test]
fn a_dependency_the_spec_needs_newer_is_updated_first() {
    let DepChoice::Update(Some(r)) = choose(false, "BuildRequires: libbar-devel >= 1.5\n") else {
        panic!("libbar should be updated first");
    };
    assert_eq!(r.requirement, "libbar-devel >= 1.5");
    assert_eq!(r.branch_package, format!("{BRANCH}/libbar"));
    assert_eq!(
        (r.origin.as_str(), r.origin_version.as_str()),
        ("devel:libraries", "1.0")
    );
}

#[test]
fn a_bound_the_origins_provide_meets_is_used_from_the_origin() {
    // libbar 1.0 is the package; its RPM provides 1.8, which meets >= 1.5.
    let spec = "BuildRequires: libbar-devel >= 1.5\n";
    let provided = vec![("libbar-devel >= 1.5".to_string(), "1.8".to_string())];
    assert!(matches!(
        choose_with(false, spec, provided),
        DepChoice::Origin(_)
    ));
    let provided = vec![("libbar-devel >= 1.5".to_string(), "1.2".to_string())];
    assert!(matches!(
        choose_with(false, spec, provided),
        DepChoice::Update(Some(_))
    ));
}

#[test]
fn by_default_a_dependency_the_origin_satisfies_is_used_from_the_origin() {
    for spec in [
        "BuildRequires: libbar-devel\n",
        "BuildRequires: libbar-devel >= 0.9\n",
        "BuildRequires: libbar-devel >= 3\n",
    ] {
        assert!(
            matches!(choose(false, spec), DepChoice::Origin(_)),
            "{spec}"
        );
    }
    let DepChoice::Origin(why) = choose(false, "BuildRequires: libbar-devel\n") else {
        unreachable!()
    };
    assert!(
        why.contains("libfoo builds against libbar 1.0 from devel:libraries"),
        "{why}"
    );
}

#[test]
fn a_bound_that_cannot_be_compared_branches_the_dependency_and_says_so() {
    let spec = "BuildRequires: libbar-devel >= %{min}\n";
    let DepChoice::UpdateUnchecked(why) = choose(false, spec) else {
        panic!("libbar should be branched with a note");
    };
    assert!(
        why.contains("libfoo needs libbar-devel >= %{min}")
            && why.contains("cannot be compared with libbar 1.0 or 2.0"),
        "{why}"
    );
    // A bound neither version meets still leaves the dependency to its origin.
    assert!(matches!(
        choose(false, "BuildRequires: libbar-devel >= 5\n"),
        DepChoice::Origin(_)
    ));
}

#[test]
fn a_dependency_with_an_unchecked_bound_is_queued_and_the_run_says_so() {
    let (mut queue, mut visited, mut count) = (VecDeque::new(), HashSet::new(), 0);
    let mut q = DepQueue {
        queue: &mut queue,
        visited: &mut visited,
        count: &mut count,
    };
    let (mut item, mut after, lines) = (libfoo(), Vec::new(), Lines::default());
    let choice = choose(false, "BuildRequires: libbar-devel >= %{min}\n");
    q.add(&mut item, libbar(), choice, &mut after, &lines);
    assert_eq!(after, [visit_key(&libbar())]);
    assert!(item.requires.is_empty());
    assert_eq!((queue.len(), count), (1, 1));
    assert!(lines.0.lock().unwrap()[0].contains("cannot be compared"));
}

#[test]
fn test_against_updates_updates_every_dependency_with_a_newer_release() {
    assert_eq!(
        choose(true, "BuildRequires: libbar-devel\n"),
        DepChoice::Update(None)
    );
    // The update the spec needs is still named.
    assert!(matches!(
        choose(true, "BuildRequires: libbar-devel = 2.0\n"),
        DepChoice::Update(Some(_))
    ));
}

#[test]
fn a_needed_dependency_is_queued_ahead_and_named_by_the_package() {
    let (mut queue, mut visited, mut count) = (VecDeque::new(), HashSet::new(), 0);
    let mut q = DepQueue {
        queue: &mut queue,
        visited: &mut visited,
        count: &mut count,
    };
    let (mut item, mut after, lines) = (libfoo(), Vec::new(), Lines::default());
    let choice = choose(false, "BuildRequires: libbar-devel >= 2\n");
    q.add(&mut item, libbar(), choice, &mut after, &lines);
    assert_eq!(after, [visit_key(&libbar())]);
    assert_eq!(item.requires.len(), 1);
    assert_eq!((queue.len(), count), (1, 1));
    assert!(lines.0.lock().unwrap()[0].contains("updating libbar to 2.0 first"));
}

#[test]
fn a_dependency_used_from_the_origin_is_not_queued_or_waited_for() {
    let (mut queue, mut visited, mut count) = (VecDeque::new(), HashSet::new(), 0);
    let mut q = DepQueue {
        queue: &mut queue,
        visited: &mut visited,
        count: &mut count,
    };
    let (mut item, mut after, lines) = (libfoo(), Vec::new(), Lines::default());
    let choice = choose(false, "BuildRequires: libbar-devel\n");
    q.add(&mut item, libbar(), choice, &mut after, &lines);
    assert!(after.is_empty() && item.requires.is_empty());
    assert_eq!((queue.len(), count), (0, 0));
}

#[test]
fn a_dependency_the_run_branches_for_another_package_is_waited_for() {
    let (mut queue, mut visited, mut count) = (VecDeque::new(), HashSet::new(), 1);
    visited.insert(visit_key(&libbar()));
    let mut q = DepQueue {
        queue: &mut queue,
        visited: &mut visited,
        count: &mut count,
    };
    let (mut item, mut after, lines) = (libfoo(), Vec::new(), Lines::default());
    let choice = choose(false, "BuildRequires: libbar-devel\n");
    q.add(&mut item, libbar(), choice, &mut after, &lines);
    assert_eq!(after, [visit_key(&libbar())]);
    assert!(queue.is_empty() && item.requires.is_empty());
    assert!(lines.0.lock().unwrap()[0].contains("waits for libbar"));
}

#[test]
fn a_needed_dependency_over_the_limit_is_not_named_as_required() {
    let (mut queue, mut visited, mut count) =
        (VecDeque::new(), HashSet::new(), MAX_DEPENDENCY_ITEMS);
    let mut q = DepQueue {
        queue: &mut queue,
        visited: &mut visited,
        count: &mut count,
    };
    let (mut item, mut after, lines) = (libfoo(), Vec::new(), Lines::default());
    let choice = choose(false, "BuildRequires: libbar-devel >= 2\n");
    q.add(&mut item, libbar(), choice, &mut after, &lines);
    assert!(queue.is_empty() && item.requires.is_empty());
    // Nothing is queued, so there is no key to wait for either.
    assert!(after.is_empty());
    assert!(lines.0.lock().unwrap()[0].starts_with("warning: --update-deps: not updating libbar"));
}

#[test]
fn a_bound_the_new_release_adds_branches_the_dependency_on_the_first_run() {
    // The spec before the update has no bound; the new release needs bar >= 1.5.
    let spec = "BuildRequires: python3-bar\n";
    let DepChoice::Update(Some(r)) = choose_predicted(spec, Some(&["bar>=1.5"])) else {
        panic!("python-bar should be updated first");
    };
    assert_eq!(
        r.requirement,
        "python3-bar >= 1.5 (PyPI's Requires-Dist of python-foo 3.0)"
    );
    // Without a prediction the spec decides, as before.
    assert!(matches!(choose_predicted(spec, None), DepChoice::Origin(_)));
}

#[test]
fn a_predicted_bound_the_origin_meets_leaves_the_dependency_to_its_origin() {
    let spec = "BuildRequires: python3-bar\n";
    let DepChoice::Origin(why) = choose_predicted(spec, Some(&["bar>=0.9", "baz>=7"])) else {
        panic!("python-bar should come from its origin");
    };
    assert!(
        why.contains("neither its BuildRequires nor PyPI's Requires-Dist of 3.0 need 2.0"),
        "{why}"
    );
    // The spec's own bound still counts.
    assert!(matches!(
        choose_predicted("BuildRequires: python3-bar >= 1.5\n", Some(&["bar>=0.9"])),
        DepChoice::Update(Some(_))
    ));
}

#[test]
fn a_predicted_bound_that_cannot_be_compared_still_branches() {
    let choice = choose_predicted("BuildRequires: python3-bar\n", Some(&["bar>=1!1.5"]));
    assert!(
        matches!(choice, DepChoice::UpdateUnchecked(_)),
        "{choice:?}"
    );
}

#[test]
fn metadata_that_cannot_be_read_falls_back_to_the_spec() {
    // An unreadable line gives no prediction, so the spec's bound decides.
    let lines: &[&str] = &["bar>=1.5", "bar ^2"];
    assert!(matches!(
        choose_predicted("BuildRequires: python3-bar\n", Some(lines)),
        DepChoice::Origin(_)
    ));
    assert!(matches!(
        choose_predicted("BuildRequires: python3-bar >= 1.5\n", Some(lines)),
        DepChoice::Update(Some(_))
    ));
}

/// What becomes of python-bar for python-foo when its spec requires it as
/// `%{python_module bar ...}`, read for python313: the search names its
/// origin by `python313-bar`.
fn choose_module(spec: &str, lines: Option<&[&str]>) -> DepChoice {
    let (bar, mut origin) = python_bar();
    origin.names = vec!["python313-bar".to_string()];
    let prediction = match lines {
        Some(lines) => {
            let json = serde_json::json!({
                "info": { "version": "3.0", "requires_dist": lines },
            })
            .to_string();
            let pypi = ap_build::update::Ecosystem::Pypi {
                pypi_name: "foo".into(),
            };
            depreach::predicted_requirements(&pypi, "3.0", |_, _| Ok(Some(json)))
        }
        None => Prediction::Unknown,
    };
    let reqs = spec_requirements(spec, &["python313".to_string()]).requirements;
    choose_dependency(
        false,
        &reqs,
        &prediction,
        &origin,
        &bar,
        "python-foo",
        BRANCH,
    )
}

#[test]
fn a_python_module_bound_is_judged_as_a_package_bound() {
    let DepChoice::Update(Some(r)) =
        choose_module("BuildRequires: %{python_module bar >= 1.5}\n", None)
    else {
        panic!("python-bar should be updated first");
    };
    assert_eq!(r.requirement, "python313-bar >= 1.5");
    for spec in [
        "BuildRequires: %{python_module bar >= 0.9}\n",
        "BuildRequires: %{python_module bar}\n",
    ] {
        let choice = choose_module(spec, None);
        assert!(matches!(choice, DepChoice::Origin(_)), "{spec}: {choice:?}");
    }
    // PyPI's bound reaches a module the spec names without one.
    assert!(matches!(
        choose_module("BuildRequires: %{python_module bar}\n", Some(&["bar>=1.5"])),
        DepChoice::Update(Some(_))
    ));
}
