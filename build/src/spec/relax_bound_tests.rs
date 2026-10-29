use super::{relax_dependencies, relax_dependency_bounds};

#[test]
fn the_else_branch_keeps_its_requires_when_the_if_branch_has_the_same_text() {
    let spec = "%if %{with foo}\nRequires: python-foo\n%else\nRequires: python-foo >= 2\n%endif\n";
    let out = relax_dependency_bounds(spec, "foo", "1.5");
    assert_eq!(
        out,
        "%if %{with foo}\nRequires: python-foo\n%else\nRequires: python-foo\n%endif\n"
    );
}

#[test]
fn a_repeat_within_one_branch_is_still_merged() {
    let spec = "%if 1\nRequires: python-foo >= 2\nRequires: python-foo >= 3\n%endif\n";
    let out = relax_dependency_bounds(spec, "foo", "1.5");
    assert_eq!(out, "%if 1\nRequires: python-foo\n%endif\n");
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
        "BuildRequires:  %{python_module trio >= 0.25.1}\n"
    );
    assert_eq!(
        relax_dependency_bounds(
            "BuildRequires:  %{python_module trio >= 1 with %python-trio < 2}\n",
            "trio",
            "3.0"
        ),
        "BuildRequires:  %{python_module trio >= 1}\n"
    );
    assert_eq!(
        relax_dependency_bounds(
            "BuildRequires:  %{python_module trio >= 5 with %python-trio < 6}\n",
            "trio",
            "3.0"
        ),
        "BuildRequires:  %{python_module trio < 6}\n"
    );
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
             BuildRequires:  %{python_module pydantic}\n\
             BuildRequires:  %{python_module redis}\n\
             BuildRequires:  %{python_module mcp == 1.27.2}\n\
             Requires:       python-pydantic\n\
             Requires:       python-redis\n\
             Requires:       python-mcp == 1.27.2\n\
             Recommends:     python-redis\n\
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
    assert!(out.contains("Requires: python-x\n"), "{out}");
    assert!(out.contains("python-y >= 1"));
    let same = "Requires: python-x >= 1\n";
    assert_eq!(relax_dependency_bounds(same, "x", "2.0"), same);
}
