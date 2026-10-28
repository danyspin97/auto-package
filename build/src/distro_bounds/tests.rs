use super::*;

fn v(s: &str) -> Version {
    parse_version(s).unwrap_or_else(|| panic!("{s} does not parse"))
}

fn meets(version: &str, spec: &str) -> bool {
    let clauses = parse_specifier(spec).unwrap_or_else(|| panic!("{spec} does not parse"));
    violation(&clauses, &v(version)).is_none()
}

fn conflict(dependent: &str, dependency: &str) -> BoundConflict {
    BoundConflict {
        dependency: dependency.into(),
        distro_version: "2.13.5".into(),
        resolved_version: "2.13.4".into(),
        dependent: dependent.into(),
        specifier: "==2.13.4".into(),
        kind: Kind::DistroTooNew,
    }
}

/// The same plan is derived every round; each (dependent, dependency) is
/// told once, however spelled, and notes once per text.
#[test]
fn a_conflict_is_reported_once_per_dependent_and_dependency() {
    let mut reported = Reported::default();
    assert!(reported.first_conflict("p:x", &conflict("open-webui", "pydantic")));
    assert!(!reported.first_conflict("p:x", &conflict("open-webui", "pydantic")));
    assert!(!reported.first_conflict(
        "p:x",
        &conflict("Open_WebUI".replace("WebUI", "webui").as_str(), "Pydantic")
    ));
    // Another dependency, another dependent, another project: new.
    assert!(reported.first_conflict("p:x", &conflict("open-webui", "redis")));
    assert!(reported.first_conflict("p:x", &conflict("vllm", "pydantic")));
    assert!(reported.first_conflict("p:y", &conflict("open-webui", "pydantic")));
    assert!(reported.first_note("p:x", "numpy 2 is older"));
    assert!(!reported.first_note("p:x", "numpy 2 is older"));
    assert!(reported.first_note("p:y", "numpy 2 is older"));
}

#[test]
fn versions_order_as_pep_440() {
    assert!(v("1.0.dev1") < v("1.0a1"));
    assert!(v("1.0a1") < v("1.0b1"));
    assert!(v("1.0b1") < v("1.0rc1"));
    assert!(v("1.0rc1") < v("1.0"));
    assert!(v("1.0") < v("1.0.post1"));
    assert!(v("0.1.11") < v("0.1.14.post1"));
    assert!(v("0.1.14.post1") > v("0.1.14"));
    assert_eq!(v("1.0"), v("1.0.0"));
    assert!(v("2.0.0") > v("1.99.99"));
    assert!(v("1!0.1") > v("5.0"));
    assert!(v("0.66b0") < v("0.66b1"));
    assert!(v("1.0-1") > v("1.0"));
    assert!(parse_version("not a version").is_none());
}

#[test]
fn the_apache_tvm_ffi_case_is_a_too_new_distro_version() {
    let clauses = parse_specifier(">=0.1.10,<=0.1.11").unwrap();
    assert_eq!(
        violation(&clauses, &v("0.1.14.post1")),
        Some(Kind::DistroTooNew)
    );
    assert_eq!(violation(&clauses, &v("0.1.9")), Some(Kind::DistroTooOld));
    assert_eq!(violation(&clauses, &v("0.1.11")), None);
}

#[test]
fn an_exact_pin_misses_in_either_direction() {
    let pin = parse_specifier("==8.0.1").unwrap();
    assert_eq!(violation(&pin, &v("7.0.1")), Some(Kind::DistroTooOld));
    assert_eq!(violation(&pin, &v("8.1.0")), Some(Kind::DistroTooNew));
    assert_eq!(violation(&pin, &v("8.0.1")), None);
    assert!(meets("1.2.7", "==1.2.*"));
    assert!(!meets("1.3.0", "==1.2.*"));
    assert!(meets("1.3.0", "!=1.2.*"));
}

#[test]
fn a_compatible_release_misses_in_either_direction() {
    // `~=0.1.0` is `>=0.1.0, ==0.1.*`: below 0.1.0 is too old, above 0.1.x
    // is too new.
    let c = parse_specifier("~=0.1.0").unwrap();
    assert_eq!(violation(&c, &v("0.0.9")), Some(Kind::DistroTooOld));
    assert_eq!(violation(&c, &v("0.2.0")), Some(Kind::DistroTooNew));
    assert_eq!(violation(&c, &v("0.1.5")), None);
    // `~=1.4` is `>=1.4, ==1.*`.
    let c = parse_specifier("~=1.4").unwrap();
    assert_eq!(violation(&c, &v("1.3")), Some(Kind::DistroTooOld));
    assert_eq!(violation(&c, &v("2.0")), Some(Kind::DistroTooNew));
    assert_eq!(violation(&c, &v("1.9")), None);
}

#[test]
fn compatible_release_and_exclusive_bounds() {
    assert!(meets("13.4.4", "~=13.4.4"));
    assert!(!meets("13.9", "~=13.4.4"));
    assert!(meets("2.5", "~=2.2"));
    assert!(!meets("3.0", "~=2.2"));
    // `<2` keeps out `2.0rc1`, which is a pre-release of 2; `>1` keeps out
    // `1.post1`, which is a post release of 1.
    assert!(!meets("2.0rc1", "<2"));
    assert!(meets("1.9", "<2"));
    assert!(!meets("1.0.post1", ">1.0"));
    assert!(meets("1.1", ">1.0"));
    assert!(meets("1.0", ">=1.0"));
}

#[test]
fn a_specifier_that_is_not_understood_is_not_held_against_a_version() {
    assert!(parse_specifier("garbage").is_none());
    assert!(parse_specifier(">=1.0.*").is_none());
    assert!(parse_specifier("(>=1.0, <2)").is_some());
    assert!(parse_version("2.0.0rc1").is_some());
}

#[test]
fn requirement_lines_split_into_name_bound_and_marker() {
    assert_eq!(
        parse_requirement("apache-tvm-ffi<=0.1.11,>=0.1.10"),
        Some((
            "apache-tvm-ffi".to_string(),
            "<=0.1.11,>=0.1.10".to_string(),
            None
        ))
    );
    assert_eq!(
        parse_requirement("uvicorn[standard] (>=0.15) ; sys_platform == \"linux\""),
        Some((
            "uvicorn".to_string(),
            "(>=0.15)".to_string(),
            Some("sys_platform == \"linux\"".to_string())
        ))
    );
    assert_eq!(
        parse_requirement("pkg @ https://example.org/pkg.whl"),
        Some(("pkg".to_string(), String::new(), None))
    );
    assert_eq!(parse_requirement("  "), None);
}

#[test]
fn markers_follow_the_linux_target() {
    assert!(marker_applies("sys_platform == \"linux\""));
    assert!(!marker_applies("sys_platform == \"win32\""));
    assert!(!marker_applies(
        "platform_system == 'Windows' and python_version < '3.10'"
    ));
    assert!(marker_applies(
        "sys_platform == \"win32\" or platform_machine == \"x86_64\""
    ));
    assert!(marker_applies("platform_machine != \"aarch64\""));
    // The Python version is not evaluated, so the bound stays in force.
    assert!(marker_applies("python_version < \"3.10\""));
    // Unreadable (parenthesised) markers apply.
    assert!(marker_applies("(sys_platform == \"win32\")"));
    // Optional dependencies never apply.
    assert!(!marker_applies("extra == \"cuda\""));
}

fn node(name: &str, version: &str, requires: &[&str]) -> Node {
    Node {
        name: name.to_string(),
        version: version.to_string(),
        sha256: String::new(),
        requires_dist: requires.iter().map(|s| s.to_string()).collect(),
        requires_python: None,
        is_requested: false,
        is_yanked: false,
    }
}

fn shipped<'a>(table: &'a [(usize, &'a str)]) -> impl FnMut(usize) -> Option<String> + 'a {
    move |i| {
        table
            .iter()
            .find(|(j, _)| *j == i)
            .map(|(_, v)| v.to_string())
    }
}

#[test]
fn a_too_new_distro_dependency_is_reported_and_stays_distro() {
    let nodes = vec![
        node("tilelang", "0.1.12", &["apache-tvm-ffi>=0.1.10,<=0.1.11"]),
        node("apache-tvm-ffi", "0.1.11", &[]),
    ];
    let plan = plan(
        &nodes,
        &[false, true],
        &[true, false],
        shipped(&[(1, "0.1.14.post1")]),
    );
    assert_eq!(plan.rebuild, Vec::<usize>::new());
    assert_eq!(plan.conflicts.len(), 1);
    let c = &plan.conflicts[0];
    assert_eq!(c.kind, Kind::DistroTooNew);
    assert_eq!(c.dependent, "tilelang");
    assert!(c.message("openSUSE:Factory").contains("convention 59"));
}

#[test]
fn a_too_old_distro_dependency_is_rebuilt_and_its_own_bounds_are_checked() {
    // open-webui pins redis==8.0.1; Factory has 7.0.1. Rebuilding redis
    // here makes redis a dependent: it needs `async-timeout>=5`, which the
    // distro ships at 4.0.
    let nodes = vec![
        node("open-webui", "0.12.0", &["redis==8.0.1"]),
        node("redis", "8.0.1", &["async-timeout>=5"]),
        node("async-timeout", "5.0.1", &[]),
    ];
    let plan = plan(
        &nodes,
        &[false, true, true],
        &[true, false, false],
        shipped(&[(1, "7.0.1"), (2, "4.0")]),
    );
    assert_eq!(plan.rebuild, vec![1, 2]);
    assert!(plan.conflicts.iter().all(|c| c.kind == Kind::DistroTooOld));
    assert!(
        plan.conflicts[0]
            .message("openSUSE:Factory")
            .contains("redis 8.0.1 is built in this project")
    );
}

#[test]
fn a_dependent_the_distro_ships_is_not_checked() {
    // numba ships from the distro: its pin on llvmlite was met by the
    // distro's own build.
    let nodes = vec![
        node("numba", "0.65.0", &["llvmlite<0.48,>=0.47"]),
        node("llvmlite", "0.47.0", &[]),
    ];
    let plan = plan(
        &nodes,
        &[true, true],
        &[false, false],
        shipped(&[(1, "0.50.0")]),
    );
    assert!(plan.conflicts.is_empty());
}

#[test]
fn unknown_versions_optional_and_foreign_bounds_are_not_reported() {
    let nodes = vec![
        node(
            "app",
            "1.0",
            &[
                "a>=2",
                "b>=2 ; extra == 'test'",
                "c>=2 ; sys_platform == 'win32'",
                "d>=not-a-version",
                "e",
            ],
        ),
        node("a", "2.0", &[]),
        node("b", "2.0", &[]),
        node("c", "2.0", &[]),
        node("d", "2.0", &[]),
        node("e", "2.0", &[]),
    ];
    // `a` has no known distro version; every other one is too old.
    let plan = plan(
        &nodes,
        &[false, true, true, true, true, true],
        &[true, false, false, false, false, false],
        shipped(&[(2, "1.0"), (3, "1.0"), (4, "1.0"), (5, "1.0")]),
    );
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
}

#[test]
fn only_bounded_distro_dependencies_of_built_nodes_are_looked_up() {
    let nodes = vec![
        node("app", "1.0", &["a>=1", "b"]),
        node("a", "1.0", &[]),
        node("b", "1.0", &[]),
        node("lib", "1.0", &["a>=1"]),
    ];
    assert_eq!(
        candidates(
            &nodes,
            &[false, true, true, false],
            &[true, false, false, false]
        ),
        vec![1]
    );
}

#[test]
fn a_requested_root_older_in_the_distro_is_called_out() {
    let root = node("vllm", "0.31.0", &[]);
    let message = stale_root_message(&root, "0.30.0", "openSUSE:Factory").unwrap();
    assert!(message.contains("vllm 0.30.0") && message.contains("--force"));
    assert_eq!(
        stale_root_message(&root, "0.31.0", "openSUSE:Factory"),
        None
    );
    assert_eq!(
        stale_root_message(&root, "0.32.0", "openSUSE:Factory"),
        None
    );
    assert_eq!(stale_root_message(&root, "git", "openSUSE:Factory"), None);
}

#[test]
fn fetch_parallel_keeps_the_answers_it_got() {
    let got = fetch_parallel(&[3, 5, 7], 2, |i| (i != 5).then(|| format!("v{i}")));
    assert_eq!(got.len(), 2);
    assert_eq!(got[&3], "v3");
    assert_eq!(got[&7], "v7");
    assert!(fetch_parallel(&[], 4, |_| Some(String::new())).is_empty());
}
