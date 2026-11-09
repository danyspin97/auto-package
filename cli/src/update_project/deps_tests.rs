use super::*;
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
    };
    let reqs = build_requirements(spec);
    choose_dependency(
        test_against_updates,
        &reqs,
        &origin,
        &libbar(),
        "libfoo",
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
    assert!(lines.0.lock().unwrap()[0].starts_with("warning: --update-deps: not updating libbar"));
}
