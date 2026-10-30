use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::*;
use crate::depclass::Availability;
use crate::osc::project_packages::PROJECT_PACKAGES_CACHE;

/// The build paths of each `(project, repository)`, in the order OBS lists them.
type Graph = HashMap<PathPair, Vec<PathPair>>;
type Listings = HashMap<String, HashSet<String>>;

const TUMBLEWEED: &str = "openSUSE_Tumbleweed";
const LEAP: &str = "16.0";

fn pair(project: &str, repository: &str) -> PathPair {
    (project.to_string(), repository.to_string())
}

/// `edges` maps a `(project, repository)` to the `(project, repository)` pairs
/// its repository builds against.
fn graph(edges: &[((&str, &str), &[(&str, &str)])]) -> Graph {
    edges
        .iter()
        .map(|((p, r), to)| (pair(p, r), to.iter().map(|(tp, tr)| pair(tp, tr)).collect()))
        .collect()
}

/// Projects whose every path is in the one repository `r`.
fn single_repo(edges: &[(&str, &[&str])]) -> Graph {
    edges
        .iter()
        .map(|(from, to)| (pair(from, "r"), to.iter().map(|p| pair(p, "r")).collect()))
        .collect()
}

fn listings(entries: &[(&str, &[&str])]) -> Listings {
    entries
        .iter()
        .map(|(p, pkgs)| (p.to_string(), pkgs.iter().map(|s| s.to_string()).collect()))
        .collect()
}

/// What a walk asked of the fake OBS: each pair whose paths were read, and each
/// project that was listed.
#[derive(Default)]
struct Calls {
    paths_read: Mutex<Vec<PathPair>>,
    listed: Mutex<Vec<String>>,
}

impl Calls {
    fn paths_read(&self, project: &str, repository: &str) -> usize {
        let wanted = pair(project, repository);
        self.paths_read
            .lock()
            .unwrap()
            .iter()
            .filter(|p| **p == wanted)
            .count()
    }

    fn listed(&self, project: &str) -> usize {
        self.listed
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.as_str() == project)
            .count()
    }
}

/// The fake walk from `root`'s `repository` over `graph`, each listed project
/// answering from `fake` (a project with no listing is an error).
fn walk(
    root: &str,
    repository: &str,
    graph: &Graph,
    fake: &Listings,
    calls: &Calls,
) -> Result<Vec<(String, String, Arc<HashSet<String>>)>, String> {
    walk_path_pairs(
        root,
        graph
            .get(&pair(root, repository))
            .cloned()
            .unwrap_or_default(),
        MAX_BUILD_PATH_DEPTH,
        MAX_BUILD_PATH_NODES,
        |p, r, _, expand| {
            calls.listed.lock().unwrap().push(p.to_string());
            let listing = fake
                .get(p)
                .cloned()
                .map(Arc::new)
                .ok_or_else(|| format!("no listing for {p}"))?;
            let paths = if expand {
                calls.paths_read.lock().unwrap().push(pair(p, r));
                graph.get(&pair(p, r)).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok::<_, String>(Some((listing, paths)))
        },
    )
}

/// The walk's answer as the availability check reads it: each project once, at
/// its first pair.
fn first_per_project(
    found: &[(String, String, Arc<HashSet<String>>)],
) -> Vec<(String, Arc<HashSet<String>>)> {
    let mut seen = HashSet::new();
    found
        .iter()
        .filter(|(p, _, _)| seen.insert(p.clone()))
        .map(|(p, _, l)| (p.clone(), Arc::clone(l)))
        .collect()
}

fn names(found: &[(String, String, Arc<HashSet<String>>)]) -> Vec<&str> {
    found.iter().map(|(p, _, _)| p.as_str()).collect()
}

fn availability_in(name: &str, found: &[(String, Arc<HashSet<String>>)]) -> Availability {
    let own = HashSet::new();
    let statuses = HashMap::new();
    crate::depclass::availability(name, &own, &statuses, found)
}

/// The X11:Wayland shape: its Tumbleweed repository builds against Factory, its
/// `16.0` one against Leap, and its `standard` one against the backports and
/// SLFO.
fn x11_wayland() -> Graph {
    graph(&[
        (("home:branch", TUMBLEWEED), &[("X11:Wayland", TUMBLEWEED)]),
        (("home:branch", LEAP), &[("X11:Wayland", LEAP)]),
        (
            ("X11:Wayland", TUMBLEWEED),
            &[("openSUSE:Factory", "snapshot")],
        ),
        (("X11:Wayland", LEAP), &[("openSUSE:Leap:16.0", "standard")]),
        (
            ("X11:Wayland", "standard"),
            &[
                ("openSUSE:Backports:SLE-16.0", "standard"),
                ("SUSE:SLFO:1.2", "standard"),
            ],
        ),
    ])
}

fn x11_wayland_listings() -> Listings {
    listings(&[
        ("X11:Wayland", &["xorg-x11-server"]),
        ("openSUSE:Factory", &["python-pycparser"]),
        ("openSUSE:Leap:16.0", &["leap-only-tool"]),
        ("openSUSE:Backports:SLE-16.0", &["backport-tool"]),
        ("SUSE:SLFO:1.2", &["slfo-tool"]),
    ])
}

#[test]
fn package_only_two_paths_away_is_available() {
    // The X11:Wayland layout: the branch builds against X11:Wayland, which
    // builds against openSUSE:Factory. pycparser is only in Factory.
    let g = single_repo(&[
        ("home:branch", &["X11:Wayland"]),
        ("X11:Wayland", &["openSUSE:Factory"]),
    ]);
    let fake = listings(&[
        ("X11:Wayland", &["xorg-x11-server"]),
        ("openSUSE:Factory", &["python-pycparser"]),
    ]);
    let calls = Calls::default();
    let found = walk("home:branch", "r", &g, &fake, &calls).unwrap();
    assert_eq!(names(&found), ["X11:Wayland", "openSUSE:Factory"]);
    assert!(matches!(
        availability_in("pycparser", &first_per_project(&found)),
        Availability::InLinked { ref project, ref pkg }
            if project == "openSUSE:Factory" && pkg == "python-pycparser"
    ));
}

#[test]
fn the_tumbleweed_target_never_sees_leap_projects() {
    // A repository-blind walk followed every repository's paths, so a Leap-only
    // package counted as in Tumbleweed's distro.
    let g = x11_wayland();
    let fake = x11_wayland_listings();
    let calls = Calls::default();
    let found = walk("home:branch", TUMBLEWEED, &g, &fake, &calls).unwrap();
    assert_eq!(names(&found), ["X11:Wayland", "openSUSE:Factory"]);
    for leap_only in [
        "openSUSE:Leap:16.0",
        "openSUSE:Backports:SLE-16.0",
        "SUSE:SLFO:1.2",
    ] {
        assert_eq!(calls.listed(leap_only), 0, "{leap_only} was listed");
    }
    assert!(matches!(
        availability_in("leap-only-tool", &first_per_project(&found)),
        Availability::Absent
    ));
}

#[test]
fn a_factory_only_package_counts_for_tumbleweed_and_not_for_leap() {
    let g = x11_wayland();
    let fake = x11_wayland_listings();
    let calls = Calls::default();
    let tumbleweed = walk("home:branch", TUMBLEWEED, &g, &fake, &calls).unwrap();
    assert!(matches!(
        availability_in("pycparser", &first_per_project(&tumbleweed)),
        Availability::InLinked { ref project, .. } if project == "openSUSE:Factory"
    ));

    let leap = walk("home:branch", LEAP, &g, &fake, &calls).unwrap();
    assert_eq!(names(&leap), ["X11:Wayland", "openSUSE:Leap:16.0"]);
    assert!(matches!(
        availability_in("pycparser", &first_per_project(&leap)),
        Availability::Absent
    ));
    assert!(matches!(
        availability_in("leap-only-tool", &first_per_project(&leap)),
        Availability::InLinked { ref project, .. } if project == "openSUSE:Leap:16.0"
    ));
}

#[test]
fn a_project_under_two_repositories_is_walked_per_repository() {
    // `shared` is reached through two repositories of `root`: each one's paths
    // are read for that repository only, and `shared` is listed once.
    let g = graph(&[
        (("root", "a"), &[("shared", "x")]),
        (("root", "b"), &[("shared", "y")]),
        (("shared", "x"), &[("only-x", "r")]),
        (("shared", "y"), &[("only-y", "r")]),
    ]);
    let fake = listings(&[("shared", &["s"]), ("only-x", &["ox"]), ("only-y", &["oy"])]);
    let calls = Calls::default();
    let found = walk("root", "a", &g, &fake, &calls).unwrap();
    assert_eq!(names(&found), ["shared", "only-x"]);
    assert!(matches!(
        availability_in("oy", &first_per_project(&found)),
        Availability::Absent
    ));
    assert_eq!(calls.listed("shared"), 1);
    assert_eq!(calls.paths_read("shared", "x"), 1);
    assert_eq!(calls.paths_read("shared", "y"), 0);
}

#[test]
fn a_cycle_in_the_paths_ends_the_walk() {
    // root -> A -> B -> {A, root, C}, C -> B: every pair is visited once.
    let g = single_repo(&[
        ("root", &["A"]),
        ("A", &["B"]),
        ("B", &["A", "root", "C"]),
        ("C", &["B"]),
    ]);
    let fake = listings(&[("A", &["a"]), ("B", &["b"]), ("C", &["c"])]);
    let calls = Calls::default();
    let found = walk("root", "r", &g, &fake, &calls).unwrap();
    assert_eq!(names(&found), ["A", "B", "C"]);
    for project in ["A", "B", "C"] {
        assert_eq!(calls.paths_read(project, "r"), 1, "paths of {project}");
        assert_eq!(calls.listed(project), 1, "listing of {project}");
    }
}

#[test]
fn the_walk_is_depth_first_in_obs_order() {
    // OBS expands root's first path (A) completely before its second (B): A's
    // own path C comes before B.
    let g = single_repo(&[("root", &["A", "B"]), ("A", &["C"])]);
    let fake = listings(&[("A", &["a"]), ("B", &["b"]), ("C", &["c"])]);
    let calls = Calls::default();
    let found = walk("root", "r", &g, &fake, &calls).unwrap();
    assert_eq!(names(&found), ["A", "C", "B"]);
}

#[test]
fn the_nearest_in_obs_order_shadows_a_deeper_one() {
    // `foo-lib` is in X11:Wayland (depth 1) and in Factory (depth 2, through
    // X11:Wayland's path). X11:Wayland is reached first, so it wins.
    let g = single_repo(&[
        ("home:branch", &["X11:Wayland"]),
        ("X11:Wayland", &["openSUSE:Factory"]),
    ]);
    let fake = listings(&[
        ("X11:Wayland", &["foo-lib"]),
        ("openSUSE:Factory", &["python-foo-lib"]),
    ]);
    let calls = Calls::default();
    let found = walk("home:branch", "r", &g, &fake, &calls).unwrap();
    assert!(matches!(
        availability_in("foo-lib", &first_per_project(&found)),
        Availability::InLinked { ref project, .. } if project == "X11:Wayland"
    ));
}

#[test]
fn the_depth_cap_stops_the_walk() {
    // A chain one level longer than the cap: the project at the cap is listed,
    // the one past it is not reached.
    let depth = MAX_BUILD_PATH_DEPTH + 2;
    let mut g = Graph::new();
    let mut fake = Listings::new();
    let mut from = "root".to_string();
    for k in 1..=depth {
        let to = format!("p{k}");
        g.insert(pair(&from, "r"), vec![pair(&to, "r")]);
        fake.insert(to.clone(), HashSet::from([format!("pkg-{k}")]));
        from = to;
    }
    let calls = Calls::default();
    let found = walk("root", "r", &g, &fake, &calls).unwrap();
    assert_eq!(found.len(), MAX_BUILD_PATH_DEPTH);
    assert_eq!(
        names(&found).last().copied(),
        Some(format!("p{MAX_BUILD_PATH_DEPTH}").as_str())
    );
    let view = first_per_project(&found);
    assert!(matches!(
        availability_in(&format!("pkg-{MAX_BUILD_PATH_DEPTH}"), &view),
        Availability::InLinked { .. }
    ));
    assert!(matches!(
        availability_in(&format!("pkg-{}", MAX_BUILD_PATH_DEPTH + 1), &view),
        Availability::Absent
    ));
    // The project at the cap is not expanded: its paths are never read.
    assert_eq!(
        calls.paths_read(&format!("p{MAX_BUILD_PATH_DEPTH}"), "r"),
        0
    );
}

#[test]
fn the_node_cap_stops_a_wide_walk() {
    // Ten direct paths, capped at three pairs visited: the first three, in order.
    let paths: Vec<String> = (0..10).map(|k| format!("p{k}")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let g = single_repo(&[("root", &refs)]);
    let calls = Calls::default();
    let found = walk_path_pairs(
        "root",
        g.get(&pair("root", "r")).cloned().unwrap_or_default(),
        MAX_BUILD_PATH_DEPTH,
        3,
        |p, _, _, _| {
            calls.listed.lock().unwrap().push(p.to_string());
            Ok::<_, String>(Some((p.to_string(), Vec::new())))
        },
    )
    .unwrap();
    assert_eq!(
        found.iter().map(|(p, _, _)| p.as_str()).collect::<Vec<_>>(),
        ["p0", "p1", "p2"]
    );
    assert_eq!(calls.listed("p3"), 0);
}

#[test]
fn each_project_is_listed_once_across_a_diamond() {
    // root -> {a, b}, a -> c, b -> c, c -> d: c is reached twice, expanded once.
    let g = single_repo(&[
        ("root", &["a", "b"]),
        ("a", &["c"]),
        ("b", &["c"]),
        ("c", &["d"]),
    ]);
    let fake = listings(&[("a", &["x"]), ("b", &["y"]), ("c", &["z"]), ("d", &["w"])]);
    let calls = Calls::default();
    let found = walk("root", "r", &g, &fake, &calls).unwrap();
    assert_eq!(names(&found), ["a", "c", "d", "b"]);
    for project in ["a", "b", "c", "d"] {
        assert_eq!(calls.listed(project), 1, "listing of {project}");
        assert_eq!(calls.paths_read(project, "r"), 1, "paths of {project}");
    }
}

#[test]
fn an_error_from_the_walk_stops_it_at_any_depth() {
    let g = single_repo(&[("root", &["a"]), ("a", &["b"])]);
    let result: Result<Vec<_>, String> = walk_path_pairs(
        "root",
        g.get(&pair("root", "r")).cloned().unwrap_or_default(),
        MAX_BUILD_PATH_DEPTH,
        MAX_BUILD_PATH_NODES,
        |p, r, _, expand| {
            if p == "b" {
                return Err(format!("could not read {p}"));
            }
            let paths = if expand {
                g.get(&pair(p, r)).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok(Some(((), paths)))
        },
    );
    assert_eq!(result.unwrap_err(), "could not read b");
}

#[test]
fn an_absent_path_project_is_tolerated_only_below_the_roots_own_paths() {
    let not_found = "osc meta prj a failed (exit 1):\nHTTP Error 404: Not Found";
    let hidden = "osc meta prj a failed (exit 1):\nHTTP Error 403: Forbidden";
    // A 404 or a 403 two levels down: the project is not there.
    assert!(absent_path_project(not_found, false, 2));
    assert!(absent_path_project(hidden, false, 3));
    // A root's own direct path still fails the walk, and so does a transient
    // failure at any depth.
    assert!(!absent_path_project(not_found, false, 1));
    assert!(!absent_path_project(hidden, false, 1));
    assert!(!absent_path_project("Operation timed out", true, 2));
    // Other statuses are a failure to look.
    assert!(!absent_path_project(
        "HTTP Error 401: Unauthorized",
        false,
        2
    ));
    assert!(!absent_path_project("HTTP Error 500: Internal", false, 2));
}

#[test]
fn an_absent_project_below_the_root_is_skipped_with_its_paths() {
    // `gone` is two levels down and does not exist (404): it is left out, and
    // the paths only it would reach are not read. `a` and `b` are still walked.
    let g = single_repo(&[("root", &["a"]), ("a", &["gone", "b"]), ("gone", &["deep"])]);
    let paths_read = Mutex::new(Vec::new());
    let found = linked_walk(
        "root",
        "r",
        Emit::quiet(),
        |p, r| {
            paths_read.lock().unwrap().push(p.to_string());
            Ok(g.get(&pair(p, r)).cloned().unwrap_or_default())
        },
        |p, depth| match p {
            "gone" if depth > 1 => Err(Error::OscFailed {
                cmd: format!("meta prj {p}"),
                code: 1,
                stderr: "HTTP Error 404: Not Found".into(),
            }),
            _ => Ok(p.to_string()),
        },
    )
    .unwrap();
    let found: Vec<&str> = found.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(found, ["a", "b"]);
    let read = paths_read.lock().unwrap();
    assert!(!read.contains(&"gone".to_string()));
    assert!(!read.contains(&"deep".to_string()));
}

#[test]
fn an_absent_direct_path_fails_the_walk() {
    let g = single_repo(&[("root", &["gone"])]);
    let result = linked_walk(
        "root",
        "r",
        Emit::quiet(),
        |p, r| Ok(g.get(&pair(p, r)).cloned().unwrap_or_default()),
        |_, _| {
            Err::<String, _>(Error::OscFailed {
                cmd: "meta prj gone".into(),
                code: 1,
                stderr: "HTTP Error 404: Not Found".into(),
            })
        },
    );
    assert!(result.is_err());
}

#[test]
fn the_real_listing_cache_serves_projects_already_listed() {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let root = "ap-test-build-paths-root";
    let near = "ap-test-build-paths-near";
    let far = "ap-test-build-paths-far";
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let mut guard = cache.lock().unwrap();
        guard.insert(near.to_string(), Arc::new(HashSet::from(["n".to_string()])));
        guard.insert(far.to_string(), Arc::new(HashSet::from(["f".to_string()])));
    }

    let g = single_repo(&[(root, &[near]), (near, &[far])]);
    let found = linked_walk(
        root,
        "r",
        Emit::quiet(),
        |p, r| Ok(g.get(&pair(p, r)).cloned().unwrap_or_default()),
        |p, _| list_project_packages(p, Emit::quiet()),
    )
    .unwrap();
    assert_eq!(names_of(&found), [near, far]);
    assert!(found[1].2.contains("f"));

    let mut guard = cache.lock().unwrap();
    guard.remove(near);
    guard.remove(far);
}

fn names_of<T>(found: &[(String, String, T)]) -> Vec<&str> {
    found.iter().map(|(p, _, _)| p.as_str()).collect()
}
