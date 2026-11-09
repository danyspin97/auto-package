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
