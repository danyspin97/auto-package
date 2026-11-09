//! Reading `%{python_module}` BuildRequires as flavored package requirements.
use super::*;
use crate::spec::{Bound, Requirement, build_requirements};

fn flavors() -> Vec<String> {
    vec!["python311".into(), "python313".into()]
}

fn package(name: &str, bound: Option<(&str, &str)>) -> Requirement {
    Requirement {
        dep: BuildRequire::Package(name.into()),
        bound: bound.map(|(op, version)| Bound {
            op: op.into(),
            version: version.into(),
        }),
    }
}

#[test]
fn the_plain_forms_give_the_name_and_the_bound() {
    assert_eq!(python_module("%{python_module foo}"), Some(("foo", None)));
    let (name, bound) = python_module("%{python_module foo >= 1.2}").unwrap();
    assert_eq!(name, "foo");
    let bound = bound.unwrap();
    assert_eq!((bound.op.as_str(), bound.version.as_str()), (">=", "1.2"));
    assert_eq!(
        python_module("%{python_module  setuptools  }"),
        Some(("setuptools", None))
    );
}

#[test]
fn a_name_keeps_its_dots_underscores_dashes_and_capitals() {
    for name in [
        "ruamel.yaml",
        "zope_interface",
        "typing-extensions",
        "Pillow",
    ] {
        let cap = format!("%{{python_module {name}}}");
        assert_eq!(python_module(&cap), Some((name, None)), "{cap}");
    }
}

#[test]
fn a_form_whose_name_cannot_be_told_is_not_read() {
    for cap in [
        "%{python_module %{pkg}-devel}",
        "%{python_module}",
        "%{python_modulefoo}",
        "%{python_module foo >= 1 if %python-base < 3.10}",
        "%{python_module foo ~ 1}",
        "%{python_module foo >=}",
        "%{python_expand foo}",
        "pkgconfig(foo)",
    ] {
        assert_eq!(python_module(cap), None, "{cap}");
    }
}

#[test]
fn a_macro_in_the_version_is_kept_for_the_decision_to_find_uncomparable() {
    let (name, bound) = python_module("%{python_module foo >= %{min}}").unwrap();
    assert_eq!(name, "foo");
    assert_eq!(bound.unwrap().version, "%{min}");
}

#[test]
fn each_module_becomes_one_package_requirement_per_flavor_in_its_place() {
    let reqs = build_requirements(
        "BuildRequires: cmake\n\
         BuildRequires: %{python_module ruamel.yaml >= 0.17}\n\
         BuildRequires: %{python_module pytest}\n\
         BuildRequires: pkgconfig(glib-2.0)\n",
    );
    let got = expand_python_modules(&reqs, &flavors());
    assert_eq!(
        got.requirements,
        vec![
            package("cmake", None),
            package("python311-ruamel.yaml", Some((">=", "0.17"))),
            package("python313-ruamel.yaml", Some((">=", "0.17"))),
            package("python311-pytest", None),
            package("python313-pytest", None),
            reqs[3].clone(),
        ]
    );
    assert_eq!(
        got.flavored,
        [
            "python311-ruamel.yaml",
            "python313-ruamel.yaml",
            "python311-pytest",
            "python313-pytest"
        ]
    );
}

#[test]
fn without_flavors_or_a_readable_form_the_capability_stays() {
    let reqs = build_requirements(
        "BuildRequires: %{python_module foo}\nBuildRequires: %{python_module %{bar}}\n",
    );
    assert_eq!(expand_python_modules(&reqs, &[]).requirements, reqs);
    let got = expand_python_modules(&reqs, &flavors());
    assert_eq!(got.requirements[2..], reqs[1..]);
    assert_eq!(got.flavored, ["python311-foo", "python313-foo"]);
}

#[test]
fn a_flavored_name_the_spec_also_writes_is_not_only_a_module() {
    let reqs = build_requirements(
        "BuildRequires: python313-foo\nBuildRequires: %{python_module foo >= 2}\n",
    );
    let got = expand_python_modules(&reqs, &["python313".into()]);
    assert_eq!(
        got.requirements,
        vec![
            package("python313-foo", None),
            package("python313-foo", Some((">=", "2")))
        ]
    );
    assert!(got.flavored.is_empty(), "{:?}", got.flavored);
}
