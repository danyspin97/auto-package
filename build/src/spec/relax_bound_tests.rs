use super::{relax_dependencies, relax_dependency_bounds};

#[test]
fn the_else_branch_keeps_its_requires_when_the_if_branch_has_the_same_text() {
    let spec = "%if %{with foo}\nRequires: python-foo\n%else\nRequires: python-foo < 2\n%endif\n";
    let out = relax_dependency_bounds(spec, "foo", "3.0");
    assert_eq!(
        out,
        "%if %{with foo}\nRequires: python-foo\n%else\n# upstream pins foo<2; relaxed because the shipped 3.0 does not satisfy it\nRequires: python-foo\n%endif\n"
    );
}

#[test]
fn a_repeat_within_one_branch_is_still_merged() {
    let spec = "%if 1\nRequires: python-foo < 2\nRequires: python-foo < 3\n%endif\n";
    let out = relax_dependency_bounds(spec, "foo", "4.0");
    assert_eq!(
        out,
        "%if 1\n# upstream pins foo<2; relaxed because the shipped 4.0 does not satisfy it\nRequires: python-foo\n%endif\n"
    );
}

#[test]
fn a_bound_the_shipped_version_misses_is_dropped_the_name_kept() {
    let spec = "BuildRequires:  %{python_module apache-tvm-ffi >= 0.1.10}\nBuildRequires:  %{python_module apache-tvm-ffi < 0.1.12}\nRequires:       python-apache-tvm-ffi >= 0.1.10, python-apache-tvm-ffi <= 0.1.11\nRequires:       python-numpy < 1\n";
    let out = relax_dependency_bounds(spec, "apache-tvm-ffi", "0.1.14.post1");
    assert!(
        out.contains("BuildRequires:  %{python_module apache-tvm-ffi >= 0.1.10}"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module apache-tvm-ffi}"),
        "{out}"
    );
    assert!(!out.contains("< 0.1.12"), "{out}");
    assert!(
        out.contains("python-apache-tvm-ffi >= 0.1.10, python-apache-tvm-ffi"),
        "{out}"
    );
    assert!(!out.contains("<= 0.1.11"), "{out}");
    assert!(out.contains("python-numpy < 1"), "{out}");
}

/// python-pycrdt: a rich `with` dependency whose upper clause the distro's
/// trio 0.34.0 misses; only the clause it misses goes.
#[test]
fn a_missed_clause_of_a_with_dependency_is_dropped_the_met_one_stays() {
    let spec = "BuildRequires:  %{python_module trio >= 0.25.1 with %python-trio < 0.34}\n";
    assert_eq!(
        relax_dependency_bounds(spec, "trio", "0.34.0"),
        "# upstream pins trio<0.34; relaxed because the shipped 0.34.0 does not satisfy it\nBuildRequires:  %{python_module trio >= 0.25.1}\n"
    );
    assert_eq!(
        relax_dependency_bounds(
            "BuildRequires:  %{python_module trio >= 1 with %python-trio < 2}\n",
            "trio",
            "3.0"
        ),
        "# upstream pins trio<2; relaxed because the shipped 3.0 does not satisfy it\nBuildRequires:  %{python_module trio >= 1}\n"
    );
    // A minimum the shipped version does not reach is not relaxed.
    let too_old = "BuildRequires:  %{python_module trio >= 5 with %python-trio < 6}\n";
    assert_eq!(relax_dependency_bounds(too_old, "trio", "3.0"), too_old);
    // Both met: untouched.
    assert_eq!(relax_dependency_bounds(spec, "trio", "0.30.0"), spec);
}

/// open-webui declares `==` pins on packages the distro ships newer:
/// BuildRequires, Requires and a rich dependency all lose the pin and keep
/// the name, whichever package's spec it is (the root included), and a pin
/// the shipped version meets stays.
#[test]
fn every_recorded_pin_is_relaxed_in_every_dependency_line() {
    let spec = "Name:           python-open-webui\n\
                    BuildRequires:  %{python_module pydantic == 2.13.4}\n\
                    BuildRequires:  %{python_module redis == 8.0.1}\n\
                    BuildRequires:  %{python_module mcp == 1.27.2}\n\
                    Requires:       python-pydantic == 2.13.4\n\
                    Requires:       python-redis = 8.0.1\n\
                    Requires:       python-mcp == 1.27.2\n\
                    Recommends:     python-redis == 8.0.1\n\
                    %python_subpackages\n";
    let relaxed = vec![
        ("pydantic".to_string(), "2.13.5".to_string()),
        ("redis".to_string(), "8.1.0".to_string()),
    ];
    let out = relax_dependencies(spec, &relaxed);
    assert_eq!(
        out,
        "Name:           python-open-webui\n\
             # upstream pins pydantic==2.13.4; relaxed because the shipped 2.13.5 does not satisfy it\n\
             BuildRequires:  %{python_module pydantic >= 2.13.4}\n\
             # upstream pins redis==8.0.1; relaxed because the shipped 8.1.0 does not satisfy it\n\
             BuildRequires:  %{python_module redis >= 8.0.1}\n\
             BuildRequires:  %{python_module mcp == 1.27.2}\n\
             # upstream pins pydantic==2.13.4; relaxed because the shipped 2.13.5 does not satisfy it\n\
             Requires:       python-pydantic >= 2.13.4\n\
             # upstream pins redis=8.0.1; relaxed because the shipped 8.1.0 does not satisfy it\n\
             Requires:       python-redis >= 8.0.1\n\
             Requires:       python-mcp == 1.27.2\n\
             # upstream pins redis==8.0.1; relaxed because the shipped 8.1.0 does not satisfy it\n\
             Recommends:     python-redis >= 8.0.1\n\
             %python_subpackages\n"
    );
    assert_eq!(relax_dependencies(spec, &[]), spec);
}

#[test]
fn an_exact_pin_is_relaxed_and_a_met_bound_stays() {
    let out = relax_dependency_bounds(
        "Requires: python-x = 1.0\nRequires: python-y >= 1\n",
        "x",
        "2.0",
    );
    assert!(
        out.contains("# upstream pins x=1.0; relaxed because the shipped 2.0 does not satisfy it\nRequires: python-x >= 1.0\n"),
        "{out}"
    );
    assert!(out.contains("python-y >= 1"));
    let same = "Requires: python-x >= 1\n";
    assert_eq!(relax_dependency_bounds(same, "x", "2.0"), same);
}

#[test]
fn a_dropped_bound_is_noted_once_without_a_macro_and_a_second_pass_changes_nothing() {
    let spec = "BuildRequires:  %{python_module numpy < 2.4}\n";
    let out = relax_dependency_bounds(spec, "numpy", "2.5.0");
    assert_eq!(
        out,
        "# upstream pins numpy<2.4; relaxed because the shipped 2.5.0 does not satisfy it\n\
         BuildRequires:  %{python_module numpy}\n"
    );
    assert!(!out.lines().next().unwrap().contains('%'));
    assert_eq!(relax_dependency_bounds(&out, "numpy", "2.5.0"), out);
    // A bound the shipped version meets leaves no comment.
    assert_eq!(relax_dependency_bounds(spec, "numpy", "2.0.0"), spec);
}

#[test]
fn a_minimum_or_a_pin_the_shipped_version_does_not_reach_is_not_relaxed() {
    // Too old: relaxing would not make the package resolvable (convention 75 (section 73)).
    for spec in [
        "Requires: python-x >= 3\n",
        "BuildRequires: %{python_module x > 3}\n",
        "Requires: python-x == 3.0\n",
    ] {
        assert_eq!(relax_dependency_bounds(spec, "x", "2.0"), spec, "{spec}");
    }
    // Too new: a pin becomes its minimum, an upper bound goes.
    assert_eq!(
        relax_dependency_bounds(
            "Requires: python-x == 1.0\nRequires: python-x < 2\n",
            "x",
            "2.5"
        ),
        "# upstream pins x==1.0; relaxed because the shipped 2.5 does not satisfy it\n\
         Requires: python-x >= 1.0\n\
         # upstream pins x<2; relaxed because the shipped 2.5 does not satisfy it\n\
         Requires: python-x\n"
    );
}
