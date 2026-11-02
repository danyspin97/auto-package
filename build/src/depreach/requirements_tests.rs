//! Capability requirements judged through the resolver, against a fake OBS:
//! the paths of the branch, the RPMs a binary search finds (the indexed
//! projects), the names a repository lists (any project), the RPMs' provides,
//! the branch's packages and the flags. Leap 16.1 is not indexed by the
//! search, as in OBS, so its RPMs are only seen through its listing.
use super::*;
use crate::spec::{Bound, Requirement};
use ap_core::sink::NullReporter;
use std::sync::atomic::{AtomicUsize, Ordering};

const OWN: &str = "home:u:branches:X11:Wayland";

pub(super) struct CapObs {
    pub(super) metas: HashMap<String, String>,
    pub(super) pkg_metas: HashMap<(String, String), String>,
    /// RPMs of the indexed projects: what the binary search finds.
    pub(super) indexed: Vec<osc::CandidateRpm>,
    /// Binary names each (project, repository) lists.
    pub(super) listings: HashMap<(String, String), Result<Vec<String>, String>>,
    /// `Provides:` of each (project, RPM name).
    pub(super) provides: HashMap<(String, String), Result<Vec<String>, String>>,
    pub(super) packages: Result<HashSet<String>, String>,
    pub(super) reads: AtomicUsize,
    pub(super) writes: AtomicUsize,
}

pub(super) fn project(name: &str, repos: &[(&str, &str, &str)]) -> String {
    // (repository, path project, path repository); an empty path project is none.
    let mut xml = format!("<project name=\"{name}\">\n");
    for (repo, path_project, path_repo) in repos {
        xml.push_str(&format!("  <repository name=\"{repo}\">\n"));
        if !path_project.is_empty() {
            xml.push_str(&format!(
                "    <path project=\"{path_project}\" repository=\"{path_repo}\"/>\n"
            ));
        }
        xml.push_str("    <arch>x86_64</arch>\n  </repository>\n");
    }
    xml.push_str("</project>\n");
    xml
}

pub(super) fn rpm(project: &str, repository: &str, name: &str) -> osc::CandidateRpm {
    osc::CandidateRpm {
        name: name.into(),
        project: project.into(),
        repository: repository.into(),
        arch: "x86_64".into(),
        package: Some(name.trim_end_matches("-devel").into()),
    }
}

impl Obs for CapObs {
    fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
        Ok(self.metas.get(p).cloned())
    }
    fn set_project_meta(&self, _: &str, _: &str) -> Result<(), String> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err("read only in tests".into())
    }
    fn published_binaries(&self, _: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        Ok(Vec::new())
    }
    fn package_meta(&self, p: &str, pkg: &str) -> Result<Option<String>, String> {
        Ok(self
            .pkg_metas
            .get(&(p.to_string(), pkg.to_string()))
            .cloned())
    }
    fn rpms_containing(
        &self,
        needle: &str,
        projects: &[String],
        _: &str,
    ) -> Result<osc::RpmSearch, String> {
        let rpms = self
            .indexed
            .iter()
            .filter(|r| r.name.contains(needle))
            .filter(|r| projects.is_empty() || projects.contains(&r.project))
            .cloned()
            .collect();
        Ok(osc::RpmSearch {
            rpms,
            truncated: false,
        })
    }
    fn rpm_provides(&self, p: &str, _: &str, _: &str, name: &str) -> Result<Vec<String>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.provides
            .get(&(p.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_else(|| Err(format!("no such binary {name} in {p}")))
    }
    fn repository_names(&self, p: &str, r: &str, _: &str) -> Result<Vec<String>, String> {
        self.listings
            .get(&(p.to_string(), r.to_string()))
            .cloned()
            .unwrap_or_else(|| Ok(Vec::new()))
    }
    fn project_packages(&self, _: &str) -> Result<HashSet<String>, String> {
        self.packages.clone()
    }
}

/// The branch builds 16.1 against Leap 16.1 and 16.0 against Leap 16.0.
/// Leap 16.0 is indexed; Leap 16.1 is listed only. `X11:Other` is not in any
/// path, though it publishes a newer hyprutils.
pub(super) fn fixture() -> CapObs {
    let mut metas = HashMap::new();
    metas.insert(
        OWN.to_string(),
        project(
            OWN,
            &[
                ("16.1", "openSUSE:Leap:16.1", "standard"),
                ("16.0", "openSUSE:Leap:16.0", "standard"),
            ],
        ),
    );
    metas.insert(
        "openSUSE:Leap:16.1".into(),
        project("openSUSE:Leap:16.1", &[("standard", "", "")]),
    );
    metas.insert(
        "openSUSE:Leap:16.0".into(),
        project("openSUSE:Leap:16.0", &[("standard", "", "")]),
    );
    metas.insert(
        "X11:Other".into(),
        project("X11:Other", &[("16.1", "", "")]),
    );
    let indexed = vec![
        rpm("openSUSE:Leap:16.0", "standard", "glib2-devel"),
        rpm("openSUSE:Leap:16.0", "standard", "pixman-devel"),
        rpm("X11:Other", "16.1", "hyprutils-devel"),
    ];
    let listings = HashMap::from([(
        ("openSUSE:Leap:16.1".to_string(), "standard".to_string()),
        Ok(vec![
            "glib2-devel".to_string(),
            "hyprutils-devel".to_string(),
        ]),
    )]);
    let provides = HashMap::from([
        (
            ("openSUSE:Leap:16.0".to_string(), "glib2-devel".to_string()),
            Ok(vec!["pkgconfig(glib-2.0) = 2.80.0".to_string()]),
        ),
        (
            ("openSUSE:Leap:16.0".to_string(), "pixman-devel".to_string()),
            Ok(vec!["pkgconfig(pixman-1) = 0.43.0".to_string()]),
        ),
        (
            ("openSUSE:Leap:16.1".to_string(), "glib2-devel".to_string()),
            Ok(vec!["pkgconfig(glib-2.0) = 2.80.0".to_string()]),
        ),
        (
            (
                "openSUSE:Leap:16.1".to_string(),
                "hyprutils-devel".to_string(),
            ),
            Ok(vec!["pkgconfig(hyprutils) = 0.6.0".to_string()]),
        ),
        (
            ("X11:Other".to_string(), "hyprutils-devel".to_string()),
            Ok(vec!["pkgconfig(hyprutils) = 0.7.2".to_string()]),
        ),
    ]);
    CapObs {
        metas,
        pkg_metas: HashMap::new(),
        indexed,
        listings,
        provides,
        packages: Ok(HashSet::new()),
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
    }
}

/// A fake shared with the test, so its counters can be read after a check.
pub(super) struct Shared(pub(super) Arc<CapObs>);

impl Obs for Shared {
    fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
        self.0.project_meta(p)
    }
    fn set_project_meta(&self, p: &str, x: &str) -> Result<(), String> {
        self.0.set_project_meta(p, x)
    }
    fn published_binaries(&self, n: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        self.0.published_binaries(n)
    }
    fn package_meta(&self, p: &str, k: &str) -> Result<Option<String>, String> {
        self.0.package_meta(p, k)
    }
    fn rpms_containing(&self, n: &str, p: &[String], a: &str) -> Result<osc::RpmSearch, String> {
        self.0.rpms_containing(n, p, a)
    }
    fn rpm_provides(&self, p: &str, r: &str, a: &str, n: &str) -> Result<Vec<String>, String> {
        self.0.rpm_provides(p, r, a, n)
    }
    fn repository_names(&self, p: &str, r: &str, a: &str) -> Result<Vec<String>, String> {
        self.0.repository_names(p, r, a)
    }
    fn project_packages(&self, p: &str) -> Result<HashSet<String>, String> {
        self.0.project_packages(p)
    }
}

pub(super) fn resolver(obs: CapObs) -> DepResolver {
    let reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(NullReporter);
    DepResolver::new(Box::new(obs), reporter, OWN, "x86_64")
}

pub(super) fn req(cap: &str, bound: Option<(&str, &str)>) -> Requirement {
    Requirement {
        dep: BuildRequire::Capability(cap.into()),
        bound: bound.map(|(op, v)| Bound {
            op: op.into(),
            version: v.into(),
        }),
    }
}

pub(super) fn verdict_with(r: &DepResolver, reqs: &[Requirement], run: &[String]) -> DepState {
    let v = r.check_requirements(reqs, "pkg", "Origin", run).unwrap();
    assert_eq!(v.len(), 1, "{v:?}");
    v[0].state.clone()
}

pub(super) fn verdict_of(r: &DepResolver, reqs: &[Requirement]) -> DepState {
    verdict_with(r, reqs, &[])
}

pub(super) fn capability(state: DepState) -> CapState {
    match state {
        DepState::Capability(c) => c,
        other => panic!("not a capability verdict: {other:?}"),
    }
}

#[test]
fn a_provider_in_every_repository_is_provided_including_an_unindexed_one() {
    let r = resolver(fixture());
    let state = capability(verdict_of(&r, &[req("pkgconfig(glib-2.0)", None)]));
    let CapState::Provided { provider } = state else {
        panic!("{state:?}");
    };
    assert_eq!(provider.project, "openSUSE:Leap:16.1");
}

#[test]
fn an_older_provider_in_the_paths_is_named_with_its_version() {
    let r = resolver(fixture());
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(pixman-1)", Some((">=", "0.46")))],
    ));
    let CapState::NoProvider {
        found,
        unknown,
        name,
    } = state
    else {
        panic!("{state:?}");
    };
    assert_eq!(name, "pixman");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].repo, "16.0");
    assert_eq!(found[0].rpm, "pixman-devel");
    assert_eq!(found[0].version, "0.43.0");
    assert_eq!(unknown.len(), 1, "{unknown:?}");
}

#[test]
fn an_older_provider_in_an_unindexed_project_is_found_through_its_listing() {
    let r = resolver(fixture());
    let state = capability(verdict_of(
        &r,
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    let CapState::NoProvider { found, .. } = state else {
        panic!("{state:?}");
    };
    // X11:Other publishes 0.7.2 but no path reaches it: only 16.1's 0.6.0 counts.
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].repo, "16.1");
    assert_eq!(found[0].project, "openSUSE:Leap:16.1");
    assert_eq!(found[0].version, "0.6.0");
}

#[test]
fn a_capability_no_candidate_is_named_for_is_unchecked() {
    for cap in ["pkgconfig(libudev)", "pkgconfig(egl)", "pkgconfig(x11)"] {
        let r = resolver(fixture());
        let state = capability(verdict_of(&r, &[req(cap, Some((">=", "1")))]));
        assert!(matches!(state, CapState::Unchecked(_)), "{cap}: {state:?}");
    }
}

#[test]
fn the_factory_standard_hit_counts_for_a_snapshot_path() {
    let mut obs = fixture();
    obs.metas.insert(
        "home:u:branches:Base:System".into(),
        project(
            "home:u:branches:Base:System",
            &[("openSUSE_Tumbleweed", "openSUSE:Factory", "snapshot")],
        ),
    );
    obs.metas.insert(
        "openSUSE:Factory".into(),
        project("openSUSE:Factory", &[("snapshot", "", "")]),
    );
    obs.indexed
        .push(rpm("openSUSE:Factory", "standard", "pixman-devel"));
    obs.provides.insert(
        ("openSUSE:Factory".into(), "pixman-devel".into()),
        Ok(vec!["pkgconfig(pixman-1) = 0.43.0".into()]),
    );
    let r = DepResolver::new(
        Box::new(obs),
        Arc::new(NullReporter),
        "home:u:branches:Base:System",
        "x86_64",
    );
    let reqs = [req("pkgconfig(pixman-1)", Some((">=", "0.46")))];
    let v = r.check_requirements(&reqs, "pkg", "Origin", &[]).unwrap();
    let CapState::NoProvider { found, .. } = capability(v[0].state.clone()) else {
        panic!("{v:?}");
    };
    assert_eq!(found[0].project, "openSUSE:Factory");
    assert_eq!(found[0].version, "0.43.0");
}

#[test]
fn an_unindexed_project_with_no_matching_name_is_not_a_warning() {
    let mut obs = fixture();
    obs.listings.insert(
        ("openSUSE:Leap:16.1".into(), "standard".into()),
        Ok(vec!["glib2-devel".into()]),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn a_listing_that_cannot_be_read_leaves_the_capability_unchecked() {
    let mut obs = fixture();
    obs.listings.insert(
        ("openSUSE:Leap:16.1".into(), "standard".into()),
        Err("HTTP Error 500".into()),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(glib-2.0)", None)],
    ));
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn a_repository_the_origin_disables_is_not_judged() {
    let mut obs = fixture();
    obs.pkg_metas.insert(
        (OWN.to_string(), "pkg".to_string()),
        "<package name=\"pkg\"><build><disable repository=\"16.0\"/></build></package>".into(),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(pixman-1)", Some((">=", "0.46")))],
    ));
    // 16.0 is left out, so the older pixman there is not a warning.
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn the_branch_project_flags_leave_a_repository_out() {
    let mut obs = fixture();
    obs.metas.insert(
        OWN.to_string(),
        format!(
            "{}",
            project(
                OWN,
                &[
                    ("16.1", "openSUSE:Leap:16.1", "standard"),
                    ("16.0", "openSUSE:Leap:16.0", "standard")
                ]
            )
        )
        .replace(
            "</project>",
            "  <build><disable repository=\"16.0\"/></build>\n</project>",
        ),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(hyprutils)", Some((">=", "0.7.1")))],
    ));
    let CapState::NoProvider { found, unknown, .. } = state else {
        panic!("{state:?}");
    };
    assert_eq!(found.len(), 1);
    assert!(unknown.is_empty(), "{unknown:?}");
}

#[test]
fn while_the_branch_has_no_package_the_origins_flags_are_read() {
    let mut obs = fixture();
    obs.pkg_metas.insert(
        ("Origin".to_string(), "pkg".to_string()),
        "<package name=\"pkg\"><build><disable repository=\"16.0\"/></build></package>".into(),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(pixman-1)", Some((">=", "0.46")))],
    ));
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn a_failed_read_is_unchecked() {
    let mut obs = fixture();
    obs.listings.insert(
        ("openSUSE:Leap:16.1".into(), "standard".into()),
        Ok(vec!["foo-devel".into()]),
    );
    obs.provides.insert(
        ("openSUSE:Leap:16.1".into(), "foo-devel".into()),
        Err("HTTP Error 500".into()),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(foo)", Some((">=", "1")))],
    ));
    match state {
        CapState::Unchecked(why) => assert!(why.contains("16.1"), "{why}"),
        other => panic!("expected unchecked, got {other:?}"),
    }
}

#[test]
fn a_macro_is_not_looked_up() {
    let state = capability(verdict_of(
        &resolver(fixture()),
        &[req("%{python_module glib}", None)],
    ));
    assert!(matches!(state, CapState::Unchecked(_)), "{state:?}");
}

#[test]
fn packages_are_not_judged_here() {
    let r = resolver(fixture());
    let v = r
        .check_requirements(
            &[
                req("pkgconfig(glib-2.0)", None),
                Requirement {
                    dep: BuildRequire::Package("cmake".into()),
                    bound: None,
                },
            ],
            "pkg",
            "Origin",
            &[],
        )
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name, "pkgconfig(glib-2.0)");
}

#[test]
fn a_bound_is_printed_with_each_requirement() {
    let r = resolver(fixture());
    let v = r
        .check_requirements(
            &[
                req("pkgconfig(pixman-1)", Some((">=", "0.40"))),
                req("pkgconfig(pixman-1)", Some(("<", "0.50"))),
            ],
            "pkg",
            "Origin",
            &[],
        )
        .unwrap();
    let names: Vec<&str> = v.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["pkgconfig(pixman-1) >= 0.40", "pkgconfig(pixman-1) < 0.50"]
    );
}

#[test]
fn a_run_package_that_only_shares_a_prefix_does_not_make_a_capability_unchecked() {
    let state = capability(verdict_with(
        &resolver(fixture()),
        &[req("pkgconfig(wayland-client)", Some((">=", "1.23")))],
        &["wayland-protocols".to_string()],
    ));
    if let CapState::Unchecked(why) = state {
        assert!(!why.contains("may be provided"), "{why}");
    }
}

#[test]
fn a_branch_package_named_for_the_word_or_its_lib_still_makes_it_unchecked() {
    for pkg in ["go", "libgobject-devel", "gobject"] {
        let mut obs = fixture();
        obs.packages = Ok(HashSet::from([pkg.to_string()]));
        let state = capability(verdict_of(
            &resolver(obs),
            &[req("pkgconfig(gobject-2.0)", Some((">=", "2.0")))],
        ));
        let expected = pkg == "gobject" || pkg == "libgobject-devel";
        match state {
            CapState::Unchecked(why) => {
                assert_eq!(why.contains("may be provided"), expected, "{pkg}: {why}")
            }
            other => assert!(!expected, "{pkg}: {other:?}"),
        }
    }
}

#[test]
fn a_package_is_named_for_a_word_by_the_word_or_its_lib_with_or_without_devel() {
    use super::requirements::may_be;
    let cap = "pkgconfig(wayland-client)";
    assert!(may_be("wayland", "wayland", cap));
    assert!(may_be("wayland-devel", "wayland", cap));
    assert!(may_be("libwayland", "wayland", cap));
    assert!(may_be("libwayland-devel", "wayland", cap));
    // The capability's own name.
    assert!(may_be("wayland-client", "wayland", cap));
    assert!(!may_be("wayland-protocols", "wayland", cap));
    assert!(!may_be("libwayland-protocols", "wayland", cap));
    assert!(!may_be("go", "gobject", "pkgconfig(gobject-2.0)"));
}

#[test]
fn a_listed_name_with_capitals_is_found_by_the_lowercase_word() {
    let mut obs = fixture();
    obs.listings.insert(
        ("openSUSE:Leap:16.1".into(), "standard".into()),
        Ok(vec!["libX11-devel".to_string()]),
    );
    obs.provides.insert(
        ("openSUSE:Leap:16.1".into(), "libX11-devel".into()),
        Ok(vec!["pkgconfig(x11) = 1.8.0".to_string()]),
    );
    let state = capability(verdict_of(
        &resolver(obs),
        &[req("pkgconfig(x11)", Some((">=", "1.9")))],
    ));
    assert!(matches!(state, CapState::NoProvider { .. }), "{state:?}");
}
