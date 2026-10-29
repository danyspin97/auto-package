use super::*;
use crate::spec::checks::*;

/// "removed duplicate BuildRequires" appears in 20 of the 25 attempt
/// journals — py2pack emits the PyPI build requirements and we then add
/// `[build-system] requires` from the sdist on top.
#[test]
fn duplicate_build_requires_collapse_to_the_constrained_line() {
    let spec = "\
BuildRequires:  %{python_module pip}
BuildRequires:  %{python_module hatchling}
BuildRequires:  %{python_module setuptools >= 42}
BuildRequires:  %{python_module hatchling >= 1.0}
BuildRequires:  %{python_module setuptools}
BuildRequires:  fdupes
";
    let out = dedupe_build_requires(spec);
    assert_eq!(out.matches("hatchling").count(), 1, "{out}");
    assert!(out.contains("%{python_module hatchling >= 1.0}"), "{out}");
    assert_eq!(out.matches("setuptools").count(), 1, "{out}");
    assert!(out.contains("%{python_module setuptools >= 42}"), "{out}");
    assert!(out.contains("fdupes"), "non-python BR dropped");
    assert!(out.contains("pip"), "pip dropped");
}

#[test]
fn dedupe_respects_conditionals_multi_package_lines_and_distinct_constraints() {
    let spec = "\
BuildRequires:  gcc make
BuildRequires:  make
BuildRequires:  foo >= 1
BuildRequires:  foo < 2
BuildRequires:  foo >= 1
%if 0%{?suse_version} > 1500
BuildRequires:  bar
%else
BuildRequires:  bar
%endif
BuildRequires:  bar
BuildRequires:  baz
%if 1
BuildRequires:  baz
BuildRequires:  baz
%endif
";
    let out = dedupe_build_requires(spec);
    assert!(out.contains("BuildRequires:  gcc make\n"), "{out}");
    assert_eq!(out.matches("make").count(), 2, "{out}");
    assert_eq!(out.matches("foo >= 1").count(), 1, "{out}");
    assert_eq!(out.matches("foo < 2").count(), 1, "{out}");
    // both branches and the top level keep their own `bar`
    assert_eq!(out.matches("bar").count(), 3, "{out}");
    // top-level `baz` plus one inside the %if
    assert_eq!(out.matches("baz").count(), 2, "{out}");
}

#[test]
fn a_non_python_buildrequires_is_not_confused_with_the_macro_form() {
    // `cmake` the RPM and `%{python_module cmake}` are different packages.
    let spec = "\
BuildRequires:  cmake
BuildRequires:  %{python_module cmake}
";
    assert_eq!(dedupe_build_requires(spec), spec);
}

/// A top-level `tests/` directory scopes the default `%pytest`
/// invocation; without one, the old unscoped `%pytest` is kept.
#[test]
fn default_check_scopes_pytest_to_a_top_level_tests_dir() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let build_system = BuildSystem {
        requires: vec![],
        backend: None,
    };
    let spec = "\
BuildRequires:  %{python_module pip}
%changelog
";
    let scoped = post_process_with_sdist(
        spec,
        &release,
        &build_system,
        SdistFacts {
            has_tests: true,
            has_c_extensions: false,
            top_level_tests_dir: Some("tests".to_string()),
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    assert!(scoped.contains("%check\n%pytest tests/\n"), "{scoped}");

    let unscoped = post_process_with_sdist(
        spec,
        &release,
        &build_system,
        SdistFacts {
            has_tests: true,
            has_c_extensions: false,
            top_level_tests_dir: None,
            install_entries: None,
            console_scripts: Vec::new(),
        },
    );
    assert!(unscoped.contains("%check\n%pytest\n"), "{unscoped}");
}
mod skeleton;
use skeleton::*;
mod requires;
mod rpmlintrc;
mod wheel_files;

/// The text of module `name`: `src/<name>.rs` and every `.rs` file under
/// `src/<name>/`, so that a module split into children is still whole.
fn module_source(name: &str) -> String {
    fn collect(dir: &std::path::Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push_str(&std::fs::read_to_string(&path).unwrap());
                out.push('\n');
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = std::fs::read_to_string(src.join(format!("{name}.rs"))).unwrap();
    collect(&src.join(name), &mut out);
    out
}

#[test]
fn dist_info_always_uses_the_normalized_name() {
    let spec = "\
%files %{python_files}
%{python_sitelib}/rich-toolkit
%{python_sitelib}/rich-toolkit-%{version}.dist-info
";
    let out = normalize_files_paths(spec, "rich-toolkit");
    assert!(out.contains("%{python_sitelib}/rich_toolkit/"), "{out}");
    assert!(
        out.contains("%{python_sitelib}/rich_toolkit-%{version}.dist-info/"),
        "{out}"
    );
}

#[test]
fn a_bare_source_filename_follows_the_real_archive() {
    let mut release = ap_ecosystem::pypi::Release {
        name: "Events".into(),
        version: "0.5".into(),
        summary: None,
        homepage: None,
        license: ap_ecosystem::pypi::License::Unknown,
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: "events-0.5.tar.gz".into(),
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    };
    let spec =
        "Source:         Events-%{version}.zip\nBuildRequires:  unzip\nBuildRequires:  fdupes\n";
    assert_eq!(
        fix_bare_source_filename(spec, &release),
        "Source:         events-%{version}.tar.gz\nBuildRequires:  fdupes\n"
    );
    // A URL names where the file comes from; leave it.
    let url = "Source:         https://files.pythonhosted.org/x/Events-%{version}.zip\n";
    assert_eq!(fix_bare_source_filename(url, &release), url);
    // Nothing to say without a known archive.
    release.sdist.filename.clear();
    assert_eq!(fix_bare_source_filename(spec, &release), spec);
}

#[test]
fn trailing_slashes_do_not_double_the_dist_info_directory() {
    let spec = "\
%files
%{python_sitelib}/accelerate/
%{python_sitelib}/accelerate-%{version}.dist-info/
";
    let out = normalize_files_paths(spec, "accelerate");
    assert_eq!(
        out,
        "%files\n%{python_sitelib}/accelerate/\n%{python_sitelib}/accelerate-%{version}.dist-info/\n"
    );
    // Idempotent: a second pass changes nothing.
    assert_eq!(normalize_files_paths(&out, "accelerate"), out);
}

#[test]
fn a_module_name_that_is_not_the_package_name_is_left_alone() {
    // PyJWT installs as `jwt`; only the agent can know that from
    // top_level.txt, so the module line must survive untouched.
    let spec = "%files %{python_files}\n%{python_sitelib}/jwt/\n";
    assert_eq!(normalize_files_paths(spec, "PyJWT"), spec);
}

#[test]
fn a_script_named_like_the_package_is_not_made_a_directory() {
    let spec = "%files %{python_files}\n%{_bindir}/black\n\
                    %python_alternative %{_bindir}/black\n\
                    %{_bindir}/black/\n%{python_sitelib}/Black\n\
                    %{$python_sitelib}/Black/\n";
    let out = normalize_files_paths(spec, "Black");
    assert!(out.contains("\n%{_bindir}/black\n"), "{out}");
    assert!(
        out.contains("\n%python_alternative %{_bindir}/black\n"),
        "{out}"
    );
    assert!(out.contains("\n%{python_sitelib}/black/\n"), "{out}");
    assert!(out.contains("\n%{$python_sitelib}/black/\n"), "{out}");
}

#[test]
fn display_name_in_files_is_normalized() {
    let spec = "%files %{python_files}\n%{python_sitearch}/MarkupSafe\n";
    let out = normalize_files_paths(spec, "MarkupSafe");
    assert!(out.contains("%{python_sitearch}/markupsafe/"), "{out}");
}

#[test]
fn only_files_sections_are_rewritten() {
    let spec = "\
%install
  echo removing foo-bar.dist-info
%files
%{python_sitelib}/foo-bar-%{version}.dist-info/
%package doc
%description doc
%{python_sitelib}/foo-bar-%{version}.dist-info/
%files doc
%{python_sitelib}/foo-bar-%{version}.dist-info/
";
    let out = normalize_files_paths(spec, "foo-bar");
    assert!(
        out.contains("%install\n  echo removing foo-bar.dist-info\n"),
        "{out}"
    );
    assert!(
        out.contains("%files\n%{python_sitelib}/foo_bar-%{version}.dist-info/\n"),
        "{out}"
    );
    assert!(
        out.contains("%description doc\n%{python_sitelib}/foo-bar-%{version}.dist-info/\n"),
        "{out}"
    );
    assert!(
        out.contains("%files doc\n%{python_sitelib}/foo_bar-%{version}.dist-info/\n"),
        "{out}"
    );
}
