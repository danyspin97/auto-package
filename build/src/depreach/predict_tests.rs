use super::*;
use crate::update::Ecosystem;

/// PyPI's JSON for `version` of a release whose `requires_dist` is `lines`.
fn json(version: &str, lines: &[&str]) -> String {
    serde_json::json!({
        "info": { "name": "pkg", "version": version, "requires_dist": lines },
        "urls": [],
    })
    .to_string()
}

fn pypi() -> Ecosystem {
    Ecosystem::Pypi {
        pypi_name: "pkg".into(),
    }
}

/// The prediction for pkg 3.0 from `lines`.
fn predict(lines: &[&str]) -> Prediction {
    predicted_requirements(&pypi(), "3.0", |name, version| {
        assert_eq!((name, version), ("pkg", "3.0"));
        Ok(Some(json("3.0", lines)))
    })
}

/// The predicted requirements, as `name op version` texts.
fn texts(prediction: &Prediction) -> Vec<String> {
    let Prediction::Requirements { requirements, .. } = prediction else {
        panic!("expected requirements, got {prediction:?}");
    };
    requirements
        .iter()
        .map(|p| {
            let BuildRequire::Package(name) = &p.requirement.dep else {
                panic!("{p:?}");
            };
            match &p.requirement.bound {
                Some(b) => format!("{name} {} {}", b.op, b.version),
                None => name.clone(),
            }
        })
        .collect()
}

#[test]
fn each_version_clause_becomes_a_bound_on_the_python3_name() {
    let p = predict(&[
        "Foo>=2.1,<3",
        "bar (~=1.4)",
        "baz==2.*",
        "qux!=1.5",
        "plain",
    ]);
    assert_eq!(
        texts(&p),
        [
            "python3-foo >= 2.1",
            "python3-foo < 3",
            "python3-bar >= 1.4",
            "python3-baz >= 2",
            "python3-plain",
        ]
    );
}

#[test]
fn extras_and_markers_false_on_linux_cpython_are_dropped() {
    let p = predict(&[
        "pytest>=8; extra == \"test\"",
        "pywin32>=300; sys_platform == \"win32\"",
        "uvloop>=0.19; sys_platform != \"win32\" and implementation_name == \"cpython\"",
        "tomli>=2; python_version < \"3.11\"",
        "rich[jupyter]>=13",
    ]);
    // A marker on the Python version is kept, as the flavours are not read.
    assert_eq!(
        texts(&p),
        [
            "python3-uvloop >= 0.19",
            "python3-tomli >= 2",
            "python3-rich >= 13"
        ]
    );
}

#[test]
fn a_name_keeps_its_dots_underscores_and_dashes_lowercased() {
    let p = predict(&["Ruamel.Yaml_Clib-Ext>=0.2"]);
    assert_eq!(texts(&p), ["python3-ruamel.yaml_clib-ext >= 0.2"]);
}

#[test]
fn a_requirement_that_cannot_be_read_gives_no_prediction() {
    for line in ["", ">=1", "foo >= $(x)", "foo ^1.2"] {
        let p = predict(&["ok>=1", line]);
        assert!(
            matches!(&p, Prediction::Failed(why) if why.contains("cannot read")),
            "{line}: {p:?}"
        );
    }
}

#[test]
fn metadata_that_does_not_describe_the_release_gives_no_prediction() {
    let fetched = |body: Result<Option<String>, String>| {
        predicted_requirements(&pypi(), "3.0", move |_, _| body)
    };
    let null = r#"{"info": {"version": "3.0", "requires_dist": null}}"#.to_string();
    for (body, why) in [
        (Ok(Some(null)), "null"),
        (Ok(Some(json("2.9", &[]))), "describes 2.9"),
        (Ok(Some("<html>".into())), "not PyPI's JSON"),
        (Ok(None), "no release 3.0 of pkg"),
        (Err("PyPI JSON for pkg 3.0: timeout".into()), "timeout"),
    ] {
        let p = fetched(body);
        assert!(
            matches!(&p, Prediction::Failed(w) if w.contains(why)),
            "{why}: {p:?}"
        );
    }
    // The same release spelled another way is still the release.
    assert!(matches!(
        predicted_requirements(&pypi(), "3.0", |_, _| Ok(Some(json("3.0.0", &[])))),
        Prediction::Requirements { .. }
    ));
}

#[test]
fn a_source_other_than_pypi_is_not_predicted_or_fetched() {
    let forge = Ecosystem::GitRepo {
        url: "https://github.com/a/b".into(),
    };
    let p = predicted_requirements(&forge, "3.0", |_, _| panic!("no fetch"));
    assert_eq!(p, Prediction::Unknown);
}

/// What `lines` (pkg 3.0's Requires-Dist) need of the dependency `dep`,
/// required by `names`, at `origin` with `new` released.
fn need_of(lines: &[&str], dep: &str, names: &[&str], origin: &str, new: &str) -> Option<DepNeed> {
    let Prediction::Requirements { requirements, .. } = predict(lines) else {
        panic!("no prediction");
    };
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    predicted_need(&requirements, dep, &names, origin, new)
}

#[test]
fn a_predicted_bound_above_the_origin_needs_the_update() {
    assert_eq!(
        need_of(&["foo>=2.1"], "python-foo", &[], "2.0", "2.4"),
        Some(DepNeed::Update("python3-foo >= 2.1".into()))
    );
}

#[test]
fn a_predicted_bound_the_origin_meets_needs_nothing() {
    assert_eq!(
        need_of(&["foo>=1.5,<3"], "python-foo", &[], "2.0", "3.1"),
        Some(DepNeed::Origin)
    );
    // A requirement that names another package says nothing of this one.
    assert_eq!(need_of(&["bar>=9"], "python-foo", &[], "2.0", "3.1"), None);
    // One that is not comparable may need it.
    assert!(matches!(
        need_of(&["foo>=1!2.0"], "python-foo", &[], "2.0", "3.1"),
        Some(DepNeed::Unknown(_))
    ));
}

#[test]
fn a_dependency_is_matched_whatever_its_separators() {
    let lines = ["Ruamel_Yaml.Clib>=0.3"];
    for dep in [
        "python-ruamel.yaml.clib",
        "python-ruamel-yaml-clib",
        "python-ruamel_yaml_clib",
    ] {
        assert!(
            matches!(
                need_of(&lines, dep, &[], "0.2", "0.3"),
                Some(DepNeed::Update(_))
            ),
            "{dep}"
        );
    }
    // A source package named otherwise is matched by the name the spec
    // gives it with a Python prefix, but not by a bare one.
    let flavoured = need_of(
        &lines,
        "ruamel-clib",
        &["python313-ruamel.yaml.clib"],
        "0.2",
        "0.3",
    );
    assert!(matches!(flavoured, Some(DepNeed::Update(_))));
    assert_eq!(
        need_of(&["cmake>=4"], "cmake", &["cmake"], "3.0", "4.1"),
        None
    );
}

#[test]
fn the_stronger_need_decides_and_the_spec_wins_a_tie() {
    let update = DepNeed::Update("python3-foo >= 2".into());
    assert_eq!(
        combined_need(DepNeed::Origin, Some(update.clone())),
        (update.clone(), true)
    );
    assert_eq!(
        combined_need(update.clone(), Some(DepNeed::Origin)),
        (update.clone(), false)
    );
    let unknown = DepNeed::Unknown("foo >= %{x}".into());
    assert_eq!(
        combined_need(DepNeed::Unmet("foo >= 9".into()), Some(unknown.clone())),
        (unknown, true)
    );
    assert_eq!(
        combined_need(DepNeed::Origin, None),
        (DepNeed::Origin, false)
    );
}
