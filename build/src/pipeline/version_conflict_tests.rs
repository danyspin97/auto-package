use super::*;

fn node(name: &str, version: &str, requires: &[&str]) -> ap_ecosystem::pip::Node {
    ap_ecosystem::pip::Node {
        name: name.into(),
        version: version.into(),
        sha256: String::new(),
        requires_dist: requires.iter().map(|s| s.to_string()).collect(),
        requires_python: None,
        is_requested: false,
        is_yanked: false,
    }
}

#[test]
fn two_roots_in_one_project_that_resolve_a_package_differently_are_warned_once() {
    let vllm = [
        node("vllm", "0.31.0", &["mcp<3.0.0,>=2.0.0"]),
        node("mcp", "2.3.0", &[]),
    ];
    let webui = [
        node("open-webui", "0.12.2", &["mcp==1.27.2"]),
        node("mcp", "1.27.2", &[]),
    ];
    assert!(version_conflicts("p:conflict", "vllm", &vllm).is_empty());
    let warnings = version_conflicts("p:conflict", "open-webui", &webui);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("mcp resolves to 2.3.0 for vllm but to 1.27.2 for open-webui"));
    assert!(
        warnings[0].contains("open-webui requires mcp==1.27.2"),
        "{}",
        warnings[0]
    );
    // Resolving again says nothing new; another project is no conflict.
    assert!(version_conflicts("p:conflict", "open-webui", &webui).is_empty());
    assert!(version_conflicts("p:other", "open-webui", &webui).is_empty());
}

/// open-webui, the requested root, pins packages the distro ships newer
/// (`pydantic==2.13.4` against 2.13.5). The check records each pin in the
/// root's own state, and the validation of the root's spec, which runs
/// before its first build, drops it from BuildRequires and Requires alike.
#[test]
fn a_root_that_pins_distro_packages_has_the_pins_relaxed_in_its_spec() {
    let mut root = node(
        "open-webui",
        "0.12.2",
        &["pydantic==2.13.4", "redis==8.0.1", "mcp==1.27.2"],
    );
    root.is_requested = true;
    let nodes = [
        root,
        node("pydantic", "2.13.4", &[]),
        node("redis", "8.0.1", &[]),
        node("mcp", "1.27.2", &[]),
    ];
    let distro = [false, true, true, false];
    let needs_build = [true, false, false, true];
    let shipped = |i: usize| match i {
        1 => Some("2.13.5".to_string()),
        2 => Some("8.0.1".to_string()),
        _ => None,
    };
    let plan = distro_bounds::plan(&nodes, &distro, &needs_build, shipped);
    assert_eq!(plan.conflicts.len(), 1, "{plan:?}");
    let conflict = &plan.conflicts[0];
    assert_eq!(conflict.dependent, "open-webui");
    assert_eq!(conflict.kind, distro_bounds::Kind::DistroTooNew);

    let mut state = PkgState::default();
    state.relax_dependency(&conflict.dependency, &conflict.distro_version);
    let spec = "BuildRequires:  %{python_module pydantic == 2.13.4}\n\
                    BuildRequires:  %{python_module redis == 8.0.1}\n\
                    Requires:       python-pydantic == 2.13.4\n\
                    Requires:       python-redis == 8.0.1\n";
    assert_eq!(
        crate::spec::relax_dependencies(spec, &state.relaxed_dependencies),
        "# upstream pins pydantic==2.13.4; relaxed because the shipped 2.13.5 does not satisfy it\n\
             BuildRequires:  %{python_module pydantic >= 2.13.4}\n\
             BuildRequires:  %{python_module redis == 8.0.1}\n\
             # upstream pins pydantic==2.13.4; relaxed because the shipped 2.13.5 does not satisfy it\n\
             Requires:       python-pydantic >= 2.13.4\n\
             Requires:       python-redis == 8.0.1\n"
    );
}
