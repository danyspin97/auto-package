use super::*;

/// `[build-system]` of configupdater 4.1.1 (its pyproject.toml, v4.1.1 tag).
const CONFIGUPDATER_411: &str = "[build-system]\n\
    requires = [\"hatchling>=1.27\", \"hatch-vcs>=0.5\"]\n\
    build-backend = \"hatchling.build\"\n";

/// The 3.2 spec the update starts from: setuptools, no minimum anywhere.
const ORIGIN_32: &str = "Name: python-ConfigUpdater\nVersion: 3.2\nRelease: 0\n\
    %python_subpackages\nBuildRequires:  %{python_module pip}\n\
    BuildRequires:  %{python_module setuptools}\nRequires:       python-configobj\n\n\
    %install\n%pyproject_install\n";

/// The same spec after the update swapped setuptools for hatchling and hatch-vcs,
/// as the agent wrote it: the names only.
const UPDATED_411: &str = "Name: python-ConfigUpdater\nVersion: 4.1.1\nRelease: 0\n\
    %python_subpackages\nBuildRequires:  %{python_module pip}\n\
    BuildRequires:  %{python_module hatch-vcs}\n\
    BuildRequires:  %{python_module hatchling}\nRequires:       python-configobj\n\n\
    %install\n%pyproject_install\n";

fn up(pyproject: &str) -> UpstreamMinimums {
    UpstreamMinimums::parse(pyproject)
}

#[test]
fn the_configupdater_update_gets_the_minimums_it_lacks() {
    let out = add_upstream_minimums(UPDATED_411, ORIGIN_32, &up(CONFIGUPDATER_411));
    assert!(
        out.contains("BuildRequires:  %{python_module hatch-vcs >= 0.5}\n"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module hatchling >= 1.27}\n"),
        "{out}"
    );
    // The line the origin has unchanged is not touched.
    assert!(
        out.contains("BuildRequires:  %{python_module pip}\n"),
        "{out}"
    );
    assert!(out.contains("Requires:       python-configobj\n"), "{out}");
}

#[test]
fn a_minimum_is_one_constraint_on_one_line() {
    let out = add_upstream_minimums(UPDATED_411, ORIGIN_32, &up(CONFIGUPDATER_411));
    assert_eq!(out.matches("hatchling").count(), 1, "{out}");
    assert!(!out.contains("<"), "{out}");
}

#[test]
fn a_dependency_upstream_declares_without_a_minimum_is_unchanged() {
    let pyproject = "[build-system]\nrequires = [\"wheel\"]\n";
    let out = add_upstream_minimums(UPDATED_411, ORIGIN_32, &up(pyproject));
    assert_eq!(out, UPDATED_411);
}

#[test]
fn an_inherited_unbounded_line_is_not_touched() {
    // hatchling is in the origin too, unchanged: the origin's line stays bare,
    // even though upstream now declares a minimum for it.
    let origin = ORIGIN_32.replace(
        "BuildRequires:  %{python_module setuptools}",
        "BuildRequires:  %{python_module hatchling}",
    );
    let out = add_upstream_minimums(UPDATED_411, &origin, &up(CONFIGUPDATER_411));
    assert!(
        out.contains("BuildRequires:  %{python_module hatchling}\n"),
        "{out}"
    );
    assert!(!out.contains("hatchling >="), "{out}");
    let gaps = inherited_minimum_gaps(UPDATED_411, &origin, &up(CONFIGUPDATER_411));
    assert_eq!(gaps, ["hatchling >= 1.27"]);
}

#[test]
fn a_name_differing_in_case_or_underscore_is_matched() {
    let spec = UPDATED_411.replace("hatch-vcs", "hatch_vcs");
    let out = add_upstream_minimums(&spec, ORIGIN_32, &up(CONFIGUPDATER_411));
    assert!(
        out.contains("BuildRequires:  %{python_module hatch_vcs >= 0.5}\n"),
        "{out}"
    );
    let spec = UPDATED_411.replace("hatchling", "Hatchling");
    let out = add_upstream_minimums(&spec, ORIGIN_32, &up(CONFIGUPDATER_411));
    assert!(
        out.contains("BuildRequires:  %{python_module Hatchling >= 1.27}\n"),
        "{out}"
    );
}

#[test]
fn a_name_the_spec_already_bounds_is_left_alone() {
    let spec = UPDATED_411.replace(
        "%{python_module hatchling}",
        "%{python_module hatchling >= 1.20}",
    );
    let out = add_upstream_minimums(&spec, ORIGIN_32, &up(CONFIGUPDATER_411));
    assert!(
        out.contains("%{python_module hatchling >= 1.20}\n"),
        "{out}"
    );
    assert!(!out.contains("hatchling >= 1.27"), "{out}");
}

#[test]
fn a_bound_on_a_requires_line_counts_for_the_name() {
    // A bound on the name anywhere in the spec (here the runtime side) means the
    // spec already bounds it, so its BuildRequires line is left bare.
    let spec = UPDATED_411.replace(
        "Requires:       python-configobj",
        "Requires:       python-hatchling >= 1.0",
    );
    let out = add_upstream_minimums(&spec, ORIGIN_32, &up(CONFIGUPDATER_411));
    assert!(out.contains("%{python_module hatchling}\n"), "{out}");
}

#[test]
fn a_runtime_requires_takes_the_project_dependency_minimum() {
    let pyproject = "[project]\nname = \"foo\"\ndependencies = [\"configobj>=5.0.6\", \"tomli\"]\n";
    let origin = "Name: python-foo\n%python_subpackages\nRequires:       python-configobj\n";
    let spec = "Name: python-foo\n%python_subpackages\nRequires:       python-configobj\n\
        Requires:       python-tomli\n";
    let out = add_upstream_minimums(spec, origin, &up(pyproject));
    // configobj is the origin's line, unchanged, and is left bare.
    assert!(out.contains("Requires:       python-configobj\n"), "{out}");
    // tomli is added by the update and carries no minimum upstream.
    assert!(out.contains("Requires:       python-tomli\n"), "{out}");
    // The same name changed in the update gets its minimum.
    let spec = "Name: python-foo\n%python_subpackages\nRequires:       python-configobj\n";
    let out = add_upstream_minimums(spec, "Name: python-foo\n", &up(pyproject));
    assert!(
        out.contains("Requires:       python-configobj >= 5.0.6\n"),
        "{out}"
    );
}

#[test]
fn a_marker_that_does_not_apply_gives_no_minimum() {
    let pyproject =
        "[project]\nname = \"foo\"\ndependencies = [\"hatchling>=1.27; python_version < '3'\"]\n";
    let out = add_upstream_minimums(UPDATED_411, ORIGIN_32, &up(pyproject));
    assert!(!out.contains("hatchling >="), "{out}");
}

#[test]
fn a_range_gives_its_lower_bound_only() {
    let pyproject = "[build-system]\nrequires = [\"setuptools>=42,<80\"]\n";
    let spec = "Name: python-foo\nBuildRequires:  %{python_module setuptools}\n";
    let out = add_upstream_minimums(spec, "Name: python-foo\n", &up(pyproject));
    assert!(
        out.contains("BuildRequires:  %{python_module setuptools >= 42}\n"),
        "{out}"
    );
    assert!(!out.contains("80"), "{out}");
}

#[test]
fn a_line_naming_several_packages_is_left_alone() {
    let spec = "Name: python-foo\nBuildRequires:  gcc %{python_module hatchling}\n";
    let out = add_upstream_minimums(spec, "Name: python-foo\n", &up(CONFIGUPDATER_411));
    assert_eq!(out, spec);
}

#[test]
fn a_missing_pyproject_declares_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        UpstreamMinimums::read(dir.path()),
        UpstreamMinimums::default()
    );
    assert_eq!(
        UpstreamMinimums::parse("not = [toml"),
        UpstreamMinimums::default()
    );
}
