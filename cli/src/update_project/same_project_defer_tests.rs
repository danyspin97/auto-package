use super::same_project::{NeedSameProject, SameNeed};
use super::same_project_defer::*;

fn not_found(dep: &str) -> NeedSameProject {
    NeedSameProject {
        dep: dep.into(),
        need: SameNeed::NotFoundYet {
            bound: format!("{dep} >= 2"),
        },
    }
}

fn needs(dep: &str) -> NeedSameProject {
    NeedSameProject {
        dep: dep.into(),
        need: SameNeed::Needs {
            new_version: "2".into(),
            bound: format!("{dep} >= 2"),
        },
    }
}

fn with(dep: &str, need: SameNeed) -> NeedSameProject {
    NeedSameProject {
        dep: dep.into(),
        need,
    }
}

fn defer(a: &str, needs: &[NeedSameProject], graph: &WaitGraph) -> Deferral {
    defer_decision(a, needs, graph, MAX_DEFERRED, false, None)
}

fn deferred_on(on: &[&str]) -> Deferral {
    Deferral::Defer {
        on: on.iter().map(|s| s.to_string()).collect(),
        note: format!("waiting for {}", on.join(", ")),
    }
}

#[test]
fn nothing_to_wait_for_proceeds_silently() {
    let g = WaitGraph::default();
    assert_eq!(defer("a", &[], &g), Deferral::Proceed(None));
    let settled = [
        needs("b"),
        with("c", SameNeed::NotNeeded),
        with(
            "d",
            SameNeed::Unmet {
                new_version: "2".into(),
                bound: "d >= 3".into(),
            },
        ),
        with("e", SameNeed::NoUpdate { bound: "e".into() }),
        with(
            "f",
            SameNeed::Unknown {
                new_version: "2".into(),
                bound: "f >= %{v}".into(),
            },
        ),
    ];
    assert_eq!(defer("a", &settled, &g), Deferral::Proceed(None));
}

#[test]
fn a_need_not_found_yet_defers_and_says_what_for() {
    let g = WaitGraph::default();
    assert_eq!(
        defer("a", &[not_found("b"), needs("c"), not_found("d")], &g),
        deferred_on(&["b", "d"])
    );
}

#[test]
fn b_found_later_releases_a() {
    let mut g = WaitGraph::default();
    let Deferral::Defer { on, .. } = defer("a", &[not_found("b")], &g) else {
        panic!("not deferred");
    };
    g.defer("a", &on);
    // The sweep found b, which is queued: a goes on, waiting for it.
    assert_eq!(
        defer("a", &[needs("b")], &g),
        Deferral::Proceed(Some("a: no longer waiting".into()))
    );
    g.release("a");
    assert_eq!(g.len(), 0);
}

#[test]
fn b_found_already_is_not_waited_for_by_deferral() {
    let g = WaitGraph::default();
    assert_eq!(defer("a", &[needs("b")], &g), Deferral::Proceed(None));
}

#[test]
fn the_end_of_the_sweep_releases_every_wait() {
    let mut g = WaitGraph::default();
    g.defer("b", &["c".into()]);
    g.defer("a", &["b".into()]);
    let got = defer_decision("a", &[not_found("x"), needs("b")], &g, 0, true, None);
    assert_eq!(
        got,
        Deferral::Proceed(Some(
            "a: the sweep has finished, no longer waiting for x, b".into()
        ))
    );
    // Released in order: c's waiter first, then its own waiter.
    assert_eq!(g.release_order(), ["b", "a"]);
}

#[test]
fn a_chain_waits_through_a_deferred_dependency() {
    // b waits for c, which the sweep has not found.
    let mut g = WaitGraph::default();
    assert_eq!(defer("b", &[not_found("c")], &g), deferred_on(&["c"]));
    g.defer("b", &["c".into()]);
    // a needs b, found but deferred, so not queued: a waits too.
    assert_eq!(defer("a", &[needs("b")], &g), deferred_on(&["b"]));
    g.defer("a", &["b".into()]);
    assert_eq!(g.release_order(), ["b", "a"]);
    // c is found: b goes first, then a.
    assert!(matches!(
        defer("b", &[needs("c")], &g),
        Deferral::Proceed(_)
    ));
    g.release("b");
    assert!(matches!(
        defer("a", &[needs("b")], &g),
        Deferral::Proceed(_)
    ));
}

#[test]
fn a_cycle_is_not_waited_into() {
    let mut g = WaitGraph::default();
    g.defer("a", &["b".into()]);
    // b, found, needs a, which waits for b.
    let got = defer("b", &[needs("a")], &g);
    assert_eq!(
        got,
        Deferral::BuildAgainstOrigin(
            "b needs the update of a, which waits for b: not waiting, b builds against a as \
             the source has it"
                .into()
        )
    );
}

#[test]
fn a_cycle_through_a_third_package_is_seen() {
    let mut g = WaitGraph::default();
    g.defer("a", &["b".into()]);
    g.defer("b", &["c".into()]);
    assert!(matches!(
        defer("c", &[needs("a")], &g),
        Deferral::BuildAgainstOrigin(_)
    ));
    // The other needs still defer it.
    assert_eq!(
        defer("c", &[needs("a"), not_found("d")], &g),
        deferred_on(&["d"])
    );
}

#[test]
fn the_cap_stops_new_deferrals_but_not_the_deferred() {
    let mut g = WaitGraph::default();
    g.defer("x", &["q".into()]);
    g.defer("y", &["q".into()]);
    let got = defer_decision("a", &[not_found("b")], &g, 2, false, None);
    assert_eq!(
        got,
        Deferral::BuildAgainstOrigin(
            "a needs the update of b, which the sweep has not found yet, but 2 packages wait \
             already: not waiting, a builds against the source's"
                .into()
        )
    );
    // One already deferred is judged again without the cap.
    assert_eq!(
        defer_decision("x", &[not_found("q")], &g, 2, false, None),
        deferred_on(&["q"])
    );
    assert_eq!(
        defer_decision("a", &[not_found("b")], &g, 3, false, None),
        deferred_on(&["b"])
    );
}

#[test]
fn the_graph_folds_spellings_and_orders_releases() {
    let mut g = WaitGraph::default();
    g.defer("Py-A", &["py_b".into()]);
    assert!(g.is_deferred("pya"));
    g.defer("py.b", &["c".into()]);
    g.defer("z", &[]);
    assert_eq!(g.release_order(), ["pyb", "pya", "z"]);
    // A cycle that should not exist is broken, not looped on.
    g.defer("c", &["pya".into()]);
    assert_eq!(g.release_order().len(), 4);
    g.release("PY-A");
    assert!(!g.is_deferred("pya"));
}

#[test]
fn a_dependency_of_another_package_is_never_deferred() {
    let g = WaitGraph::default();
    let got = defer_decision(
        "d",
        &[not_found("b"), needs("c")],
        &g,
        MAX_DEFERRED,
        false,
        Some("a"),
    );
    assert_eq!(
        got,
        Deferral::BuildAgainstOrigin(
            "d is needed by a, which waits for it: not waiting for b, d builds against the \
             source's"
                .into()
        )
    );
    // Nothing to wait for: it goes on silently, as any other package.
    assert_eq!(
        defer_decision("d", &[], &g, MAX_DEFERRED, false, Some("a")),
        Deferral::Proceed(None)
    );
}
