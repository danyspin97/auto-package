//! The limits of capability checks, against the same fake OBS as
//! `requirements_tests`: the run's packages and the branch's, the budget, the
//! reads per repository, candidate order, and that no build path is added.
use super::requirements_tests::*;
use super::*;
use ap_core::sink::NullReporter;
use std::sync::atomic::Ordering;
use std::time::Duration;

const OWN: &str = "home:u:branches:X11:Wayland";
const BUDGET_SPENT: &str = "capability check budget spent";

/// A clock the tests move: the fake OBS advances it by each call's cost, so
/// the time a fetch takes is exact.
#[derive(Clone, Default)]
struct Clock(Arc<Mutex<Duration>>);

impl Clock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }
    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

/// What each kind of OBS call takes on the clock.
#[derive(Clone, Copy, Default)]
struct Costs {
    meta: Duration,
    search: Duration,
    listing: Duration,
    provides: Duration,
}

/// The fake, moving the clock by the cost of each call.
struct Timed {
    inner: Shared,
    clock: Clock,
    costs: Costs,
}

impl Obs for Timed {
    fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
        self.clock.advance(self.costs.meta);
        self.inner.project_meta(p)
    }
    fn set_project_meta(&self, p: &str, x: &str) -> Result<(), String> {
        self.inner.set_project_meta(p, x)
    }
    fn published_binaries(&self, n: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        self.inner.published_binaries(n)
    }
    fn package_meta(&self, p: &str, k: &str) -> Result<Option<String>, String> {
        self.clock.advance(self.costs.meta);
        self.inner.package_meta(p, k)
    }
    fn rpms_containing(&self, n: &str, p: &[String], a: &str) -> Result<osc::RpmSearch, String> {
        self.clock.advance(self.costs.search);
        self.inner.rpms_containing(n, p, a)
    }
    fn rpm_provides(&self, p: &str, r: &str, a: &str, n: &str) -> Result<Vec<String>, String> {
        self.clock.advance(self.costs.provides);
        self.inner.rpm_provides(p, r, a, n)
    }
    fn repository_names(&self, p: &str, r: &str, a: &str) -> Result<Vec<String>, String> {
        self.clock.advance(self.costs.listing);
        self.inner.repository_names(p, r, a)
    }
    fn project_packages(&self, p: &str) -> Result<HashSet<String>, String> {
        self.clock.advance(self.costs.meta);
        self.inner.project_packages(p)
    }
}

/// A resolver over `obs` whose capability fetches are timed by the returned
/// clock.
fn timed(obs: &Arc<CapObs>, costs: Costs) -> (DepResolver, Clock) {
    let clock = Clock::default();
    let fake = Timed {
        inner: Shared(obs.clone()),
        clock: clock.clone(),
        costs,
    };
    let reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(NullReporter);
    let measure = clock.clone();
    let r = DepResolver::new(Box::new(fake), reporter, OWN, "x86_64")
        .with_capability_clock(Box::new(move || measure.now()));
    (r, clock)
}

#[test]
fn time_outside_the_capability_fetches_is_not_charged() {
    // Each meta read takes ten minutes, far past the budget, but none of it
    // is inside a capability fetch.
    let obs = Arc::new(fixture());
    let costs = Costs {
        meta: Duration::from_secs(600),
        ..Costs::default()
    };
    let (r, clock) = timed(&obs, costs);
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    assert!(clock.now() >= Duration::from_secs(600));
    assert!(matches!(state, CapState::NoProvider { .. }), "{state:?}");
}

#[test]
fn time_inside_the_capability_fetches_adds_up_across_capabilities() {
    // Each provides read takes 50 s. Glib's two reads take 100 s, under the
    // 120 s budget; pixman's read takes the total past it.
    let obs = Arc::new(fixture());
    let costs = Costs {
        provides: Duration::from_secs(50),
        ..Costs::default()
    };
    let (r, _) = timed(&obs, costs);
    let glib = capability(verdict_of(
        &r,
        &[req("pkgconfig(glib-2.0)", Some((">=", "2.70")))],
    ));
    assert!(matches!(glib, CapState::Provided { .. }), "{glib:?}");
    let pixman = capability(verdict_of(
        &r,
        &[req("pkgconfig(pixman-1)", Some((">=", "0.46")))],
    ));
    assert_eq!(pixman, CapState::Unchecked(BUDGET_SPENT.into()));
}

#[test]
fn a_listing_is_charged_once_for_its_batch() {
    // Two repositories are listed, 50 s each: 100 s in all, under the budget.
    // Charged a second time by the batch, they would spend it before the RPM
    // read, and the older hyprutils would be unchecked, not a warning.
    let obs = Arc::new(fixture());
    let costs = Costs {
        listing: Duration::from_secs(50),
        ..Costs::default()
    };
    let (r, _) = timed(&obs, costs);
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    assert!(matches!(state, CapState::NoProvider { .. }), "{state:?}");
}

#[test]
fn a_search_that_spends_the_budget_leaves_its_capability_unchecked() {
    let obs = Arc::new(fixture());
    let costs = Costs {
        search: Duration::from_secs(200),
        ..Costs::default()
    };
    let (r, _) = timed(&obs, costs);
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(glib-2.0)", Some((">=", "2.70")))],
    ));
    assert_eq!(state, CapState::Unchecked(BUDGET_SPENT.into()));
}

#[test]
fn the_budget_is_checked_between_batches_of_reads() {
    // Eight RPMs in each of two repositories make two batches of reads. The
    // fake clock adds each 50 s read as it runs, so the first batch spends the
    // budget and the second is not started.
    let mut fake = fixture();
    let names: Vec<String> = (0..8).map(|i| format!("wayland-{i}-devel")).collect();
    for n in &names {
        fake.indexed.push(rpm("openSUSE:Leap:16.0", "standard", n));
        fake.provides.insert(
            ("openSUSE:Leap:16.0".into(), n.clone()),
            Ok(vec!["pkgconfig(wayland-client) = 1.20.0".into()]),
        );
        fake.provides.insert(
            ("openSUSE:Leap:16.1".into(), n.clone()),
            Ok(vec!["pkgconfig(wayland-client) = 1.20.0".into()]),
        );
    }
    fake.listings.insert(
        ("openSUSE:Leap:16.1".into(), "standard".into()),
        Ok(names.clone()),
    );
    let obs = Arc::new(fake);
    let costs = Costs {
        provides: Duration::from_secs(50),
        ..Costs::default()
    };
    let (r, _) = timed(&obs, costs);
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(wayland-client)", Some((">=", "1.23")))],
    ));
    assert_eq!(state, CapState::Unchecked(BUDGET_SPENT.into()));
    assert_eq!(
        obs.reads.load(Ordering::SeqCst),
        FETCH_PARALLELISM,
        "the second batch was read"
    );
}

#[test]
fn a_package_of_this_run_may_provide_it_and_is_unchecked() {
    let r = resolver(fixture());
    let state = capability(verdict_with(
        &r,
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
        &["hyprutils".to_string()],
    ));
    match state {
        CapState::Unchecked(why) => assert!(why.contains("updated in this run"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_package_of_the_branch_may_provide_it_and_is_unchecked() {
    let mut obs = fixture();
    obs.packages = Ok(HashSet::from(["hyprutils".to_string()]));
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    match state {
        CapState::Unchecked(why) => assert!(why.contains(OWN), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_branch_listing_that_fails_leaves_everything_unchecked() {
    let mut obs = fixture();
    obs.packages = Err("could not list packages".into());
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(glib-2.0)", None)],
    ));
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn the_budget_spent_leaves_every_capability_unchecked() {
    let r = resolver(fixture()).with_capability_budget(Duration::ZERO);
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    match state {
        CapState::Unchecked(why) => assert_eq!(why, "capability check budget spent"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_more_than_the_reads_per_repository_are_made_and_an_unread_one_is_not_a_warning() {
    let mut obs = fixture();
    let names: Vec<String> = (0..12).map(|i| format!("wayland-{i}-devel")).collect();
    for n in &names {
        obs.provides.insert(
            ("openSUSE:Leap:16.1".into(), n.clone()),
            Ok(vec!["pkgconfig(wayland-client) = 1.20.0".into()]),
        );
    }
    obs.listings.insert(
        ("openSUSE:Leap:16.1".into(), "standard".into()),
        Ok(names.clone()),
    );
    let obs = Arc::new(obs);
    let r = DepResolver::new(
        Box::new(Shared(obs.clone())),
        Arc::new(NullReporter),
        OWN,
        "x86_64",
    );
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(wayland-client)", Some((">=", "1.23")))],
    ));
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
    // Eight reads for 16.1 at most; 16.0 reaches none of these.
    assert!(
        obs.reads.load(Ordering::SeqCst) <= CAPABILITY_READS,
        "{:?}",
        obs.reads
    );
}

#[test]
fn the_order_of_the_candidates_does_not_change_the_verdict() {
    let mut a = fixture();
    a.indexed
        .push(rpm("openSUSE:Leap:16.0", "standard", "glib2-devel-2"));
    a.provides.insert(
        ("openSUSE:Leap:16.0".into(), "glib2-devel-2".into()),
        Ok(vec!["pkgconfig(glib-2.0) = 2.66.0".into()]),
    );
    let mut b = fixture();
    b.indexed
        .insert(0, rpm("openSUSE:Leap:16.0", "standard", "glib2-devel-2"));
    b.indexed
        .push(rpm("openSUSE:Leap:16.0", "standard", "glib2-devel"));
    b.provides.insert(
        ("openSUSE:Leap:16.0".into(), "glib2-devel-2".into()),
        Ok(vec!["pkgconfig(glib-2.0) = 2.66.0".into()]),
    );
    let reqs = [req("pkgconfig(glib-2.0)", Some((">=", "2.70")))];
    let va = verdict_of(&resolver(a), &reqs);
    let vb = verdict_of(&resolver(b), &reqs);
    assert_eq!(va, vb);
}

#[test]
fn checking_a_capability_adds_no_build_path() {
    let obs = Arc::new(fixture());
    let reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(NullReporter);
    let r = DepResolver::new(Box::new(Shared(obs.clone())), reporter, OWN, "x86_64");
    let _ = verdict_of(&r, &[req("pkgconfig(pixman-1)", Some((">=", "0.46")))]);
    let _ = verdict_of(&r, &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))]);
    // Every write is refused by the fake; none was attempted.
    assert_eq!(obs.writes.load(Ordering::SeqCst), 0);
}
