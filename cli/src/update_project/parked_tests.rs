use super::parked::*;
use super::same_project_defer::WaitGraph;

fn names(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

#[test]
fn a_parked_package_is_found_under_any_spelling_of_its_name() {
    let mut p = Parked::default();
    p.park("libFoo-devel", 1);
    assert!(!p.is_empty());
    assert_eq!(p.get_mut("libfoo_devel"), Some(&mut 1));
    assert_eq!(p.take("LIBFOO.DEVEL"), Some(1));
    assert!(p.is_empty());
    assert_eq!(p.take("libfoo-devel"), None);
}

#[test]
fn parking_again_replaces_the_value_and_keeps_the_place() {
    let mut p = Parked::default();
    p.park("a", 1);
    p.park("b", 2);
    p.park("a", 3);
    assert_eq!(p.into_values(), vec![3, 2]);
}

#[test]
fn nothing_is_released_while_every_package_still_waits() {
    let mut p = Parked::default();
    p.park("a", "A");
    let order = names(&["a"]);
    assert!(release_first(&mut p, &order, |_, _| None::<()>).is_none());
    assert!(!p.is_empty());
}

#[test]
fn the_first_free_package_of_the_order_is_released_first() {
    // `b` is first in the wait order (`a` waits for it), though `a` was
    // parked first: the order given decides, and `judge` skips `b` here.
    let mut graph = WaitGraph::default();
    graph.defer("a", &["b".to_string()]);
    graph.defer("b", &[]);
    assert_eq!(graph.release_order(), names(&["b", "a"]));
    let mut p = Parked::default();
    p.park("a", "A");
    p.park("b", "B");
    let got = release_first(&mut p, &graph.release_order(), |key, _| {
        (key != "b").then_some("free")
    });
    assert_eq!(got, Some(("a".to_string(), "A", "free")));
    assert_eq!(p.into_values(), vec!["B"]);
}

#[test]
fn at_sweep_end_every_package_is_released_in_wait_order() {
    let mut graph = WaitGraph::default();
    graph.defer("c", &["b".to_string()]);
    graph.defer("b", &["a".to_string()]);
    graph.defer("a", &[]);
    let mut p = Parked::default();
    p.park("c", 'c');
    p.park("b", 'b');
    p.park("a", 'a');
    let mut released = Vec::new();
    while let Some((key, value, ())) =
        release_first(&mut p, &graph.release_order(), |_, _| Some(()))
    {
        graph.release(&key);
        released.push(value);
    }
    assert_eq!(released, vec!['a', 'b', 'c']);
    assert!(p.is_empty());
    assert!(graph.release_order().is_empty());
}

#[test]
fn a_run_that_stops_lists_the_parked_packages_in_park_order() {
    let mut p = Parked::default();
    p.park("x", 1);
    p.park("y", 2);
    p.park("z", 3);
    assert_eq!(p.take("y"), Some(2));
    assert_eq!(p.into_values(), vec![1, 3]);
}
