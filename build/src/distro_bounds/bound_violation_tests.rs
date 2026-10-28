use super::*;

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_newer_version_than_the_upper_bound_is_too_new() {
    let req = lines(&["apache-tvm-ffi<=0.1.11,>=0.1.10", "numpy>=1.23"]);
    let (kind, spec) = bound_violation(&req, "apache-tvm-ffi", "0.1.14.post1").unwrap();
    assert_eq!(kind, Kind::DistroTooNew);
    assert_eq!(spec, "<=0.1.11,>=0.1.10");
}

#[test]
fn an_older_version_than_the_lower_bound_is_too_old() {
    let req = lines(&["opentelemetry-instrumentation==0.66b1"]);
    let (kind, _) = bound_violation(&req, "opentelemetry_instrumentation", "0.66b0").unwrap();
    assert_eq!(kind, Kind::DistroTooOld);
}

#[test]
fn a_met_bound_an_extra_and_no_bound_are_none() {
    let req = lines(&["a>=1", "b>=5; extra == \"test\"", "c"]);
    assert!(bound_violation(&req, "a", "2.0").is_none());
    assert!(bound_violation(&req, "b", "1.0").is_none());
    assert!(bound_violation(&req, "c", "1.0").is_none());
}
