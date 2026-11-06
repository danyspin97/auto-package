use super::*;
use ap_core::sink::NullReporter;

fn meta(name: &str, repos: &[(&str, &[(&str, &str)])]) -> String {
    let mut xml = format!("<project name=\"{name}\">\n");
    for (repo, paths) in repos {
        xml.push_str(&format!("  <repository name=\"{repo}\">\n"));
        for (p, r) in *paths {
            xml.push_str(&format!("    <path project=\"{p}\" repository=\"{r}\"/>\n"));
        }
        xml.push_str("    <arch>x86_64</arch>\n  </repository>\n");
    }
    xml.push_str("</project>\n");
    xml
}

fn bin(name: &str, project: &str, repo: &str, package: &str) -> PublishedBinary {
    PublishedBinary {
        name: name.into(),
        project: project.into(),
        repository: repo.into(),
        arch: "x86_64".into(),
        package: Some(package.into()),
        version: Some("1".into()),
    }
}

struct Fake {
    metas: Mutex<HashMap<String, String>>,
    binaries: Vec<PublishedBinary>,
    searches: Mutex<usize>,
}

impl Obs for Fake {
    fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
        Ok(self.metas.lock().unwrap().get(p).cloned())
    }
    fn set_project_meta(&self, p: &str, xml: &str) -> Result<(), String> {
        self.metas.lock().unwrap().insert(p.into(), xml.into());
        Ok(())
    }
    fn package_meta(&self, _: &str, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn rpms_containing(&self, _: &str, _: &[String], _: &str) -> Result<osc::RpmSearch, String> {
        Ok(osc::RpmSearch::default())
    }
    fn rpm_provides(&self, _: &str, _: &str, _: &str, _: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn repository_names(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn project_packages(&self, _: &str) -> Result<HashSet<String>, String> {
        Ok(HashSet::new())
    }
    fn published_binaries(&self, names: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        *self.searches.lock().unwrap() += 1;
        Ok(self
            .binaries
            .iter()
            .filter(|b| names.contains(&b.name.as_str()))
            .cloned()
            .collect())
    }
}

const BRANCH: &str = "home:u:branches:Base:System";

/// The branch builds against Base:System, which builds against Factory.
/// `libfoo` is published by devel:languages:c_c++, which nothing reaches.
fn fixture() -> (Fake, Arc<dyn Reporter + Send + Sync>) {
    let mut metas = HashMap::new();
    metas.insert(
        BRANCH.to_string(),
        meta(
            BRANCH,
            &[
                (
                    "openSUSE_Tumbleweed",
                    &[("Base:System", "openSUSE_Tumbleweed")],
                ),
                ("15.6", &[("Base:System", "15.6")]),
            ],
        ),
    );
    metas.insert(
        "Base:System".into(),
        meta(
            "Base:System",
            &[
                ("openSUSE_Tumbleweed", &[("openSUSE:Factory", "snapshot")]),
                ("15.6", &[("openSUSE:Leap:15.6", "standard")]),
            ],
        ),
    );
    metas.insert(
        "openSUSE:Factory".into(),
        meta("openSUSE:Factory", &[("snapshot", &[])]),
    );
    metas.insert(
        "openSUSE:Leap:15.6".into(),
        meta("openSUSE:Leap:15.6", &[("standard", &[])]),
    );
    let binaries = vec![
        bin("cmake", "openSUSE:Factory", "standard", "cmake"),
        bin("cmake", "openSUSE:Leap:15.6", "standard", "cmake"),
        bin("cmake", "devel:tools", "openSUSE_Tumbleweed", "cmake"),
        bin(
            "libfoo-devel",
            "devel:languages:c_c++",
            "openSUSE_Tumbleweed",
            "libfoo",
        ),
        bin("libfoo-devel", "devel:languages:c_c++", "15.6", "libfoo"),
        bin(
            "libfoo-devel",
            "home:other:wip",
            "openSUSE_Tumbleweed",
            "libfoo",
        ),
        bin(
            "libbar-devel",
            "devel:languages:c_c++",
            "openSUSE_Tumbleweed",
            "libbar",
        ),
        bin("libbar-devel", "devel:languages:c_c++", "15.6", "libbar"),
        bin(
            "libbar-devel",
            "devel:other",
            "openSUSE_Tumbleweed",
            "libbar",
        ),
        bin("libbar-devel", "devel:other", "15.6", "libbar"),
        bin("onlyhome", "home:x", "openSUSE_Tumbleweed", "onlyhome"),
        bin("wrongrepo", "devel:z", "Other_Name", "wrongrepo"),
    ];
    (
        Fake {
            metas: Mutex::new(metas),
            binaries,
            searches: Mutex::new(0),
        },
        Arc::new(NullReporter),
    )
}

fn pkg(n: &str) -> BuildRequire {
    BuildRequire::Package(n.into())
}

fn resolver() -> (DepResolver, Arc<Fake>) {
    struct Shared(Arc<Fake>);
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
        fn package_meta(&self, p: &str, pkg: &str) -> Result<Option<String>, String> {
            self.0.package_meta(p, pkg)
        }
        fn rpms_containing(
            &self,
            n: &str,
            projects: &[String],
            arch: &str,
        ) -> Result<osc::RpmSearch, String> {
            self.0.rpms_containing(n, projects, arch)
        }
        fn rpm_provides(
            &self,
            p: &str,
            r: &str,
            arch: &str,
            name: &str,
        ) -> Result<Vec<String>, String> {
            self.0.rpm_provides(p, r, arch, name)
        }
        fn repository_names(&self, p: &str, r: &str, arch: &str) -> Result<Vec<String>, String> {
            self.0.repository_names(p, r, arch)
        }
        fn project_packages(&self, p: &str) -> Result<HashSet<String>, String> {
            self.0.project_packages(p)
        }
    }
    let (fake, reporter) = fixture();
    let fake = Arc::new(fake);
    (
        DepResolver::new(Box::new(Shared(fake.clone())), reporter, BRANCH, "x86_64"),
        fake,
    )
}

fn state<'a>(v: &'a [DepVerdict], name: &str) -> &'a DepState {
    &v.iter().find(|v| v.name == name).unwrap().state
}

#[test]
fn a_dependency_a_path_chain_publishes_is_reachable_through_the_paths_of_paths() {
    let (r, _) = resolver();
    let v = r.check(&[pkg("cmake")]).unwrap();
    // Found in Factory (via Base:System) and Leap, not devel:tools, which
    // nothing reaches.
    match state(&v, "cmake") {
        DepState::Reachable { provider } => {
            assert_eq!(provider.project, "openSUSE:Factory");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_dependency_in_an_unreached_project_is_missing_with_per_repository_options() {
    let (r, _) = resolver();
    let v = r.check(&[pkg("libfoo-devel")]).unwrap();
    let DepState::Missing { options } = state(&v, "libfoo-devel") else {
        panic!("{v:?}");
    };
    let repos: Vec<&str> = options.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(repos, ["openSUSE_Tumbleweed", "15.6"]);
    // Another user's home project is never offered.
    assert!(
        options[0]
            .1
            .iter()
            .all(|h| h.project == "devel:languages:c_c++"),
        "{options:?}"
    );
}

#[test]
fn names_nobody_can_usefully_provide_are_classified_as_such() {
    let (r, _) = resolver();
    let v = r
        .check(&[
            pkg("onlyhome"),
            pkg("wrongrepo"),
            pkg("nothing-publishes-this"),
            // Capabilities are judged by `check_requirements`, not here.
            BuildRequire::Capability("pkgconfig(x)".into()),
        ])
        .unwrap();
    assert!(matches!(
        state(&v, "onlyhome"),
        DepState::OnlyUnusable { .. }
    ));
    assert!(matches!(
        state(&v, "wrongrepo"),
        DepState::OnlyUnusable { .. }
    ));
    assert_eq!(state(&v, "nothing-publishes-this"), &DepState::NotPublished);
    assert!(v.iter().all(|v| v.name != "pkgconfig(x)"));
    assert!(choose_paths(&v).is_empty());
}

#[test]
fn the_project_covering_most_dependencies_is_chosen_first_and_once_per_repository() {
    let (r, _) = resolver();
    let v = r
        .check(&[pkg("libfoo-devel"), pkg("libbar-devel")])
        .unwrap();
    let adds = choose_paths(&v);
    // devel:languages:c_c++ covers both in both repositories (libbar also
    // has devel:other, which would be a second project for nothing).
    assert_eq!(
        adds,
        vec![
            PathAddition {
                repository: "15.6".into(),
                project: "devel:languages:c_c++".into(),
                path_repository: "15.6".into()
            },
            PathAddition {
                repository: "openSUSE_Tumbleweed".into(),
                project: "devel:languages:c_c++".into(),
                path_repository: "openSUSE_Tumbleweed".into()
            },
        ]
    );
}

#[test]
fn adding_the_chosen_paths_edits_the_meta_once_and_is_idempotent() {
    let (r, fake) = resolver();
    let v = r.check(&[pkg("libfoo-devel")]).unwrap();
    let adds = choose_paths(&v);
    let made = r.add_paths(&adds).unwrap();
    assert_eq!(made.len(), 2);
    let info = osc::parse_project_info(&fake.metas.lock().unwrap()[BRANCH]);
    for repo in &info.repositories {
        assert_eq!(
            repo.paths.last().unwrap().0,
            "devel:languages:c_c++",
            "{repo:?}"
        );
        assert_eq!(repo.paths.len(), 2);
    }
    // Reachable now, so nothing more to add; and adding again is a no-op.
    let v = r.check(&[pkg("libfoo-devel")]).unwrap();
    assert!(matches!(
        state(&v, "libfoo-devel"),
        DepState::Reachable { .. }
    ));
    assert!(r.add_paths(&adds).unwrap().is_empty());
}

#[test]
fn an_added_path_makes_a_branch_rebuild_locally_but_not_an_in_place_source() {
    let (r, fake) = resolver();
    let adds = choose_paths(&r.check(&[pkg("libfoo-devel")]).unwrap());
    r.add_paths(&adds).unwrap();
    let branch = fake.metas.lock().unwrap()[BRANCH].clone();
    let repos = osc::repository_elements(&branch).unwrap().len();
    assert_eq!(
        branch.matches("rebuild=\"local\"").count(),
        repos,
        "{branch}"
    );

    let (r, fake) = resolver();
    let r = r.for_in_place_source();
    let adds = choose_paths(&r.check(&[pkg("libfoo-devel")]).unwrap());
    assert_eq!(r.add_paths(&adds).unwrap().len(), 2);
    let source = fake.metas.lock().unwrap()[BRANCH].clone();
    assert!(!source.contains("rebuild="), "{source}");
    assert!(source.contains("devel:languages:c_c++"), "{source}");
}

#[test]
fn a_frozen_resolver_refuses_a_new_path_and_leaves_the_meta_alone() {
    let (r, fake) = resolver();
    let r = r.with_frozen_paths("a maintenance run adds no build paths");
    let before = fake.metas.lock().unwrap()[BRANCH].clone();
    let v = r.check(&[pkg("libfoo-devel")]).unwrap();
    let err = r.add_paths(&choose_paths(&v)).unwrap_err();
    assert!(
        err.starts_with(
            "a maintenance run adds no build paths; it would need devel:languages:c_c++/"
        ),
        "{err}"
    );
    assert_eq!(fake.metas.lock().unwrap()[BRANCH], before);
    assert!(r.add_paths(&[]).unwrap().is_empty());
    assert!(r.cover_dependency(&["libfoo-devel".into()]).is_err());
}

#[test]
fn a_name_is_searched_for_once_per_run() {
    let (r, fake) = resolver();
    r.check(&[pkg("cmake"), pkg("libfoo-devel")]).unwrap();
    r.check(&[pkg("cmake")]).unwrap();
    r.check(&[pkg("libfoo-devel"), pkg("cmake")]).unwrap();
    assert_eq!(*fake.searches.lock().unwrap(), 1);
}

#[test]
fn dependencies_are_sent_to_the_search_in_chunks() {
    let (r, fake) = resolver();
    let deps: Vec<BuildRequire> = (0..45).map(|i| pkg(&format!("p{i}"))).collect();
    r.check(&deps).unwrap();
    assert_eq!(*fake.searches.lock().unwrap(), 3);
}

#[test]
fn covering_a_missing_dependency_adds_a_path_instead_of_building_it() {
    let (r, fake) = resolver();
    let note = r
        .cover_dependency(&["libfoo-devel".into(), "python-libfoo-devel".into()])
        .unwrap()
        .expect("covered");
    assert!(note.contains("devel:languages:c_c++"), "{note}");
    let info = osc::parse_project_info(&fake.metas.lock().unwrap()[BRANCH]);
    assert!(
        info.repositories[0]
            .paths
            .iter()
            .any(|p| p.0 == "devel:languages:c_c++")
    );
    // A second report of it finds it reachable and edits nothing.
    let before = fake.metas.lock().unwrap()[BRANCH].clone();
    let note = r
        .cover_dependency(&["libfoo-devel".into()])
        .unwrap()
        .unwrap();
    assert!(note.contains("already builds against"), "{note}");
    assert_eq!(fake.metas.lock().unwrap()[BRANCH], before);
}

#[test]
fn a_dependency_nothing_publishes_is_left_for_the_pipeline_to_build() {
    let (r, fake) = resolver();
    let before = fake.metas.lock().unwrap()[BRANCH].clone();
    assert_eq!(
        r.cover_dependency(&["python-nope".into(), "nope".into()])
            .unwrap(),
        None
    );
    assert_eq!(r.cover_dependency(&["onlyhome".into()]).unwrap(), None);
    assert_eq!(fake.metas.lock().unwrap()[BRANCH], before);
}

#[test]
fn a_path_cycle_does_not_hang_the_walk() {
    let (r, fake) = resolver();
    fake.set_project_meta(
        "openSUSE:Factory",
        &meta(
            "openSUSE:Factory",
            &[("snapshot", &[("Base:System", "openSUSE_Tumbleweed")])],
        ),
    )
    .unwrap();
    fake.set_project_meta(
        "Base:System",
        &meta(
            "Base:System",
            &[
                ("openSUSE_Tumbleweed", &[("openSUSE:Factory", "snapshot")]),
                ("15.6", &[("openSUSE:Leap:15.6", "standard")]),
            ],
        ),
    )
    .unwrap();
    let v = r.check(&[pkg("cmake")]).unwrap();
    assert!(matches!(state(&v, "cmake"), DepState::Reachable { .. }));
}

#[test]
fn only_non_distro_non_personal_projects_are_candidates_for_updating_a_dependency() {
    let (r, _) = resolver();
    let v = r
        .check(&[pkg("cmake"), pkg("libfoo-devel"), pkg("onlyhome")])
        .unwrap();
    let adds = choose_paths(&v);
    let origins = dependency_origins(&v, &adds, BRANCH, "Base:System");
    // cmake is Factory's (a distro project), onlyhome is personal.
    assert_eq!(
        origins,
        vec![("devel:languages:c_c++".to_string(), "libfoo".to_string())]
    );
    // The project being updated from is not an origin of its own dependency.
    assert!(dependency_origins(&v, &adds, BRANCH, "devel:languages:c_c++").is_empty());
}

#[test]
fn a_branch_without_a_repository_for_the_arch_is_an_error() {
    let (fake, reporter) = fixture();
    let r = DepResolver::new(Box::new(fake), reporter, BRANCH, "aarch64");
    assert!(r.check(&[pkg("cmake")]).unwrap_err().contains("aarch64"));
}

#[test]
fn a_dry_run_sees_the_branch_a_branch_would_create_and_edits_nothing() {
    let (fake, reporter) = fixture();
    let source_info = osc::parse_project_info(&fake.metas.lock().unwrap()["Base:System"].clone());
    fake.metas.lock().unwrap().remove(BRANCH);
    let obs = DryRunObs {
        inner: fake,
        branch_project: BRANCH.into(),
        branch_meta: synthetic_branch_meta(BRANCH, "Base:System", &source_info),
    };
    let r = DepResolver::new(Box::new(obs), reporter, BRANCH, "x86_64");
    let v = r.check(&[pkg("cmake"), pkg("libfoo-devel")]).unwrap();
    // The branch would build against Base:System, so Factory is reached
    // through it, and libfoo's project is not.
    assert!(matches!(state(&v, "cmake"), DepState::Reachable { .. }));
    assert!(matches!(
        state(&v, "libfoo-devel"),
        DepState::Missing { .. }
    ));
    let adds = choose_paths(&v);
    assert_eq!(adds.len(), 2);
    assert!(r.add_paths(&adds).is_err());
}

#[test]
fn the_synthetic_meta_points_each_repository_at_the_source() {
    let info = osc::parse_project_info(&meta("S", &[("r1", &[("x", "y")]), ("r2", &[])]));
    let xml = synthetic_branch_meta("B", "S", &info);
    let got = osc::parse_project_info(&xml);
    assert_eq!(got.repositories.len(), 2);
    assert_eq!(
        got.repositories[0].paths,
        [("S".to_string(), "r1".to_string())]
    );
    assert_eq!(got.repositories[1].arches, ["x86_64"]);
}

#[test]
fn a_name_that_cannot_be_searched_is_unchecked_and_never_cached_as_unpublished() {
    let (r, fake) = resolver();
    let v = r.check(&[pkg("a'b"), pkg("cmake"), pkg("x\"y")]).unwrap();
    assert_eq!(state(&v, "a'b"), &DepState::Unchecked);
    assert_eq!(state(&v, "x\"y"), &DepState::Unchecked);
    assert!(matches!(state(&v, "cmake"), DepState::Reachable { .. }));
    // Only the searchable name was looked up, and only it is cached.
    assert_eq!(*fake.searches.lock().unwrap(), 1);
    let cache = r.binaries.lock().unwrap();
    assert!(cache.contains_key("cmake"));
    assert!(!cache.contains_key("a'b"));
    assert!(!cache.contains_key("x\"y"));
    drop(cache);
    // Such a name is left for the build, not offered a path.
    assert_eq!(r.cover_dependency(&["a'b".into()]).unwrap(), None);
}

#[test]
fn the_synthetic_meta_escapes_names_and_stays_well_formed() {
    let info = ProjectInfo {
        kind: None,
        locked: false,
        repositories: vec![osc::RepoDef {
            name: "r\"<x".into(),
            paths: vec![],
            arches: vec!["x86_64".into()],
        }],
    };
    let xml = synthetic_branch_meta("B\"<", "S&<", &info);
    assert!(xml.contains("<project name=\"B&quot;&lt;\">"), "{xml}");
    assert!(xml.contains("<repository name=\"r&quot;&lt;x\">"), "{xml}");
    assert!(
        xml.contains("<path project=\"S&amp;&lt;\" repository=\"r&quot;&lt;x\"/>"),
        "{xml}"
    );
    // The raw name does not open an element of its own, and the parser
    // hands back the original name.
    let got = osc::parse_project_info(&xml);
    assert_eq!(got.repositories.len(), 1);
    assert_eq!(got.repositories[0].name, "r\"<x");
    assert_eq!(got.repositories[0].arches, ["x86_64"]);
}

#[test]
fn an_ampersand_in_a_name_round_trips_without_double_escaping() {
    let info = ProjectInfo {
        kind: None,
        locked: false,
        repositories: vec![osc::RepoDef {
            name: "a&amp;b".into(),
            paths: vec![],
            arches: vec!["x86_64".into()],
        }],
    };
    let xml = synthetic_branch_meta("B&C", "S", &info);
    assert!(xml.contains("<repository name=\"a&amp;amp;b\">"), "{xml}");
    let got = osc::parse_project_info(&xml);
    assert_eq!(got.repositories[0].name, "a&amp;b");
    assert_eq!(got.repositories[0].arches, ["x86_64"]);
}

#[test]
fn distro_projects_are_recognised() {
    assert!(is_distro_project("openSUSE:Factory"));
    assert!(is_distro_project("SUSE:SLE-15-SP6:GA"));
    assert!(!is_distro_project("devel:languages:python"));
    assert!(!is_distro_project("Base:System"));
}
