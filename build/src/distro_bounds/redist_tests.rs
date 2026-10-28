use super::*;
use crate::redist::TestMode;

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

/// `nvidia-cublas>=13.1` is a bound on a wheel; `cuda-cublas` 13.8.0.4 is
/// NVIDIA's scheme, so the bound is never held against it (and a bound the
/// other way, `<13`, would not rebuild the component either).
#[test]
fn a_bound_on_a_cuda_wheel_is_not_compared_with_the_components_version() {
    let req = vec!["nvidia-cublas>=13.1".to_string(), "numpy>=2".to_string()];
    let nodes = vec![
        node("torch", "2.12", &["nvidia-cublas>=13.1", "numpy>=2"]),
        node("nvidia-cublas", "13.1.1.3", &[]),
        node("numpy", "2.3", &[]),
    ];
    let table = |i: usize| match i {
        1 => Some("9.0".to_string()),
        _ => Some("1.0".to_string()),
    };
    {
        let _off = TestMode::set(false);
        assert!(bound_violation(&req, "nvidia-cublas", "9.0").is_some());
        let p = plan(&nodes, &[false, true, true], &[true, false, false], table);
        assert!(p.conflicts.iter().any(|c| c.dependency == "nvidia-cublas"));
    }
    let _on = TestMode::set(true);
    assert!(bound_violation(&req, "nvidia-cublas", "9.0").is_none());
    assert!(candidates(&nodes, &[false, true, true], &[true, false, false]) == vec![2]);
    let p = plan(&nodes, &[false, true, true], &[true, false, false], table);
    assert!(p.conflicts.iter().all(|c| c.dependency != "nvidia-cublas"));
    assert!(p.rebuild.iter().all(|&i| i != 1));
    // Other bounds are still checked.
    assert!(bound_violation(&req, "numpy", "1.0").is_some());
}
