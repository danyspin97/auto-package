use super::*;

fn bound(op: &str, version: &str) -> Option<Bound> {
    Some(Bound {
        op: op.into(),
        version: version.into(),
    })
}

#[test]
fn spaced_bound_is_kept_with_its_capability() {
    let spec = "BuildRequires:  pkgconfig(miral) >= 5.5 cmake\n";
    assert_eq!(
        build_requirements(spec),
        [
            Requirement {
                dep: BuildRequire::Capability("pkgconfig(miral)".into()),
                bound: bound(">=", "5.5"),
            },
            Requirement {
                dep: BuildRequire::Package("cmake".into()),
                bound: None,
            },
        ]
    );
}

#[test]
fn inline_bound_is_split_from_the_capability() {
    let spec = "BuildRequires: pkgconfig(glib-2.0)>=2.5 foo<1\n";
    assert_eq!(
        build_requires(spec),
        [
            BuildRequire::Capability("pkgconfig(glib-2.0)".into()),
            BuildRequire::Package("foo".into()),
        ]
    );
    assert_eq!(build_requirements(spec)[0].bound, bound(">=", "2.5"));
    assert_eq!(build_requirements(spec)[1].bound, bound("<", "1"));
}

#[test]
fn operator_inside_a_macro_is_not_a_bound() {
    let spec = "BuildRequires: %{python_module foo >= 1}\n";
    let reqs = build_requirements(spec);
    assert_eq!(
        reqs[0].dep,
        BuildRequire::Capability("%{python_module foo >= 1}".into())
    );
    assert_eq!(reqs[0].bound, None);
}

#[test]
fn same_capability_with_two_bounds_is_listed_twice() {
    let spec = "BuildRequires: pkgconfig(x) >= 1\nBuildRequires: pkgconfig(x) < 2\n";
    let reqs = build_requirements(spec);
    assert_eq!(reqs.len(), 2);
    assert_eq!(build_requires(spec).len(), 1);
}
