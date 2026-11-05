use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn a_host_never_has_more_than_the_limit_in_flight() {
    let gate = Gate::new(2);
    let peak = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..10 {
            scope.spawn(|| {
                let _permit = gate.enter("pypi.org");
                peak.fetch_max(gate.in_flight("pypi.org"), Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
            });
        }
    });
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(gate.in_flight("pypi.org"), 0);
}

#[test]
fn hosts_have_their_own_slots() {
    let gate = Gate::new(1);
    let a = gate.enter("a.example");
    let b = gate.enter("b.example");
    assert_eq!(gate.in_flight("a.example"), 1);
    assert_eq!(gate.in_flight("b.example"), 1);
    drop((a, b));
    assert_eq!(gate.in_flight("a.example"), 0);
}

#[test]
fn the_host_of_a_url_is_its_authority() {
    assert_eq!(
        host_of("https://github.com/o/r/archive/v1.tar.gz"),
        "github.com"
    );
    assert_eq!(host_of("https://user@code.example:8443/x"), "code.example");
    assert_eq!(
        host_of("git@gitlab.example:group/proj.git"),
        "gitlab.example"
    );
    assert_eq!(host_of("pypi.org"), "pypi.org");

    assert_eq!(host_of("https://api.github.com/repos/o/r"), "github.com");
    assert_eq!(
        host_of("https://files.pythonhosted.org/packages/x"),
        "pypi.org"
    );
}

#[test]
fn a_permit_is_given_back_when_its_holder_panics() {
    let gate = Gate::new(1);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _permit = gate.enter("pypi.org");
        panic!("the check failed");
    }));
    assert!(panicked.is_err());
    assert_eq!(gate.in_flight("pypi.org"), 0);
    let _again = gate.enter("pypi.org");
    assert_eq!(gate.in_flight("pypi.org"), 1);
}
