use super::*;
use crate::osc::spec_info::*;
use crate::osc::unresolvable::*;

#[test]
fn a_provider_choice_line_is_read() {
    let detail = vec![
            "have choice for libnccl.so.2()(64bit) needed by python314-nccl-extensions: python313-nvidia-nccl-cu13 python314-nvidia-nccl-cu13".to_string(),
        ];
    let c = provider_choices(&detail);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].capability, "libnccl.so.2()(64bit)");
    assert_eq!(c[0].needed_by, "python314-nccl-extensions");
    assert_eq!(
        c[0].providers,
        ["python313-nvidia-nccl-cu13", "python314-nvidia-nccl-cu13"]
    );
}

#[test]
fn a_package_at_an_older_version_is_outdated_not_missing() {
    let detail = vec![
        "nothing provides python313-opentelemetry-instrumentation >= 0.66b1".to_string(),
        "(got version 0.66b0-1.1)".to_string(),
        "nothing provides python314-opentelemetry-instrumentation >= 0.66b1".to_string(),
        "(got version 0.66b0-1.1)".to_string(),
        "nothing provides python313-logfire".to_string(),
    ];
    assert_eq!(
        outdated_packages(&detail),
        [(
            "opentelemetry-instrumentation".to_string(),
            Some("0.66b1".to_string())
        )]
    );
}

/// `osc results --verbose` (no build log exists yet) joins the constraint
/// and the version OBS has on one line; python-detect-installer looped on
/// `uv-build < 0.10.0` against the distro's 0.11.7 because that form was
/// not read.
#[test]
fn a_version_miss_on_one_line_is_read_like_one_on_two() {
    let detail = vec![
        "nothing provides python314-uv-build < 0.10.0, (got version 0.11.7-1.4)".to_string(),
        "nothing provides python313-uv-build < 0.10.0, (got version 0.11.7-1.4)".to_string(),
    ];
    let misses = version_misses(&detail);
    assert_eq!(misses.len(), 1);
    assert_eq!(misses[0].name, "uv-build");
    assert_eq!(misses[0].got, "0.11.7");
    assert!(misses[0].too_new);
}

/// python-pycrdt: OBS says `nothing provides (python314-trio >= 0.25.1 with
/// python314-trio < 0.34)` while the distro ships trio 0.34.0.
#[test]
fn a_rich_dependency_whose_bound_the_distro_misses_becomes_a_version_miss() {
    let detail = vec![
        "nothing provides (python314-trio >= 0.25.1 with python314-trio < 0.34)".to_string(),
        "nothing provides python313-other".to_string(),
    ];
    let shipped = |n: &str| (n == "trio").then(|| "0.34.0".to_string());
    let out = annotate_rich_misses(detail, &shipped);
    assert_eq!(
        out,
        [
            "nothing provides python314-trio < 0.34",
            "(got version 0.34.0-0)",
            "nothing provides python313-other",
        ]
    );
    let misses = version_misses(&out);
    assert_eq!(misses.len(), 1);
    assert!(misses[0].too_new);
    assert_eq!(misses[0].name, "trio");
    // A version that meets every clause, or one that is unknown, leaves the line.
    let met = |_: &str| Some("0.30.0".to_string());
    let line =
        vec!["nothing provides (python314-trio >= 0.25.1 with python314-trio < 0.34)".to_string()];
    assert_eq!(annotate_rich_misses(line.clone(), &met), line);
    assert_eq!(annotate_rich_misses(line.clone(), &|_: &str| None), line);
}

/// Regression test: a rich `(A or B)` requires group leaves a stray `)`
/// on the last alternative's "nothing provides" line, and a multibuild
/// flavor annotation leaves a trailing `[flavor]`. Either one turns the
/// extracted name into something neither `strip_rpm_prefix` nor a PyPI
/// lookup recognizes, so the dependency silently drops out of the queue
/// instead of being built.
#[test]
fn strips_boolean_group_parens_and_flavor_brackets() {
    let log = "\
unresolvable: nothing provides python314-pytest-rerunfailures
unresolvable: nothing provides python313-duckdb)
nothing provides python314-pybind11[global] needed by python314-fastar
nothing provides (python313-foo
";
    let names = extract_unresolvable(log);
    assert_eq!(
        names,
        vec![
            "python314-pytest-rerunfailures",
            "python313-duckdb",
            "python314-pybind11",
            "python313-foo",
        ]
    );
}

/// A rich dependency's `if` is a keyword, never a package.
#[test]
fn rich_dependency_keywords_are_not_package_names() {
    let log = "\
nothing provides (python313-tomli >= 2.0.0 if python313-base < 3.11) needed by python313-fastapi-cli
nothing provides if python313-base < 3.11
";
    assert_eq!(
        extract_unresolvable(log),
        vec!["python313-tomli", "python313-base"]
    );
}

#[test]
fn leaves_ordinary_names_untouched() {
    assert_eq!(
        clean_unresolvable_name("python314-duckdb"),
        "python314-duckdb"
    );
}

/// The `()(64bit)` of a soname capability is part of the name; stripping
/// the trailing `)` turned it into something no lookup recognizes.
#[test]
fn keeps_the_parentheses_of_a_soname_capability() {
    assert_eq!(
        extract_unresolvable("nothing provides libcuda.so.1()(64bit) needed by python313-torch"),
        vec!["libcuda.so.1()(64bit)"]
    );
    assert_eq!(
        clean_unresolvable_name("pkgconfig(zlib)"),
        "pkgconfig(zlib)"
    );
}

/// The `<details>` of python-fastapi-cloud-cli on i586 in
/// home:dspinella:open-webui, verbatim: every requirement on one line.
const FASTAPI_CLOUD_CLI_DETAILS: &str = "nothing provides python314-fastar, nothing provides python313-fastar, nothing provides python314-rignore, nothing provides python313-rignore, nothing provides python314-sentry-sdk, nothing provides python313-sentry-sdk";

/// A joined details line used to yield only `python314-fastar,` (comma
/// attached), so five of the six names never reached the queue.
#[test]
fn reads_every_requirement_on_a_joined_details_line() {
    assert_eq!(
        extract_unresolvable(FASTAPI_CLOUD_CLI_DETAILS),
        vec![
            "python314-fastar",
            "python313-fastar",
            "python314-rignore",
            "python313-rignore",
            "python314-sentry-sdk",
            "python313-sentry-sdk",
        ]
    );
    let detail = extract_unresolvable_detail(FASTAPI_CLOUD_CLI_DETAILS);
    assert_eq!(detail.len(), 6);
    assert_eq!(detail[0], "nothing provides python314-fastar");
}

#[test]
fn joined_requirements_keep_needed_by_and_version_out_of_the_name() {
    let line = "unresolvable: nothing provides python314-a >= 2 needed by python314-x, \
                    nothing provides python314-b needed by python314-y";
    assert_eq!(
        extract_unresolvable(line),
        vec!["python314-a", "python314-b"]
    );
    // A line that only mentions the phrase is not a requirement.
    assert!(extract_unresolvable("error: nothing provides python314-a").is_empty());
}

#[test]
fn solver_conflicts_travel_with_the_detail_but_name_no_package() {
    let log = "\
unresolvable: have choice for python3-foo needed by python314-bar: python314-foo python313-foo
unresolvable: nothing provides python314-setuptools < 70.0.0
(got version 80.9.0-3.3)
conflict for providers of python314-baz needed by python314-qux
";
    assert_eq!(extract_unresolvable(log), vec!["python314-setuptools"]);
    assert_eq!(
        extract_unresolvable_detail(log),
        vec![
            "unresolvable: have choice for python3-foo needed by python314-bar: python314-foo python313-foo",
            "unresolvable: nothing provides python314-setuptools < 70.0.0",
            "(got version 80.9.0-3.3)",
            "conflict for providers of python314-baz needed by python314-qux",
        ]
    );
}

#[test]
fn blockers_collapse_flavors_to_one_package_each() {
    assert_eq!(
        blockers_from_details(FASTAPI_CLOUD_CLI_DETAILS),
        vec!["fastar", "rignore", "sentry-sdk"]
    );
}

#[test]
fn a_shared_library_capability_is_never_a_package_to_build() {
    let details = "nothing provides libcuda.so.1()(64bit) needed by python314-foo, \
                       nothing provides python314-bar";
    assert_eq!(blockers_from_details(details), vec!["bar"]);
    let names = vec![
        "libcuda.so.1()(64bit)".to_string(),
        "libnvidia-ml.so.1()(64bit)".to_string(),
        "/usr/bin/nvcc".to_string(),
        "python313-beautifulsoup4".to_string(),
    ];
    assert_eq!(missing_package_names(&names), vec!["beautifulsoup4"]);
}

#[test]
fn capability_names_keep_their_parentheses_when_extracted() {
    assert_eq!(
        extract_unresolvable("nothing provides libcuda.so.1()(64bit) needed by python314-x"),
        vec!["libcuda.so.1()(64bit)"]
    );
    assert!(is_rpm_capability("libcuda.so.1"));
    assert!(!is_rpm_capability("python313-duckdb)"));
    assert!(!is_rpm_capability("libsodium-devel"));
}

#[test]
fn blockers_skip_version_conflicts_and_interpreter_pieces() {
    let details = "nothing provides python314-setuptools < 70.0.0, \
                       nothing provides python3-devel, \
                       nothing provides python-rpm-macros, \
                       nothing provides python314-ninja, \
                       nothing provides libfoo-devel, \
                       nothing provides python313-Foo_Bar, \
                       nothing provides python314-foo-bar";
    assert_eq!(blockers_from_details(details), vec!["Foo_Bar", "foo"]);
}

/// The spec OBS holds for python-cuda-bindings (13.4.3-2.1), verbatim
/// apart from the licence header: the monorepo skeleton, never finished.
/// Its build "succeeded" and published two ~11 KB RPMs with no modules.
const CUDA_BINDINGS_SPEC: &str = "\
Name:           python-cuda-bindings
Version:        13.4.3
Release:        0
Summary:        Python bindings for CUDA
License:        Apache-2.0
BuildRequires:  python-rpm-macros
BuildRequires:  %{python_module pip}
%python_subpackages

# This is a monorepo: all subprojects below must become %%package sections.
# TODO: add %package -n python-%{name}-cuda-bindings for cuda_bindings/
%description
Python bindings for CUDA

%prep
%autosetup -p1 -n cuda-python-13.4.3

%build
# TODO: build each subproject in dependency order

%install
# TODO: install each subproject

%check
# TODO: check each subproject

%files %{python_files}
%license LICENSE

%changelog
";

#[test]
fn the_unfinished_cuda_bindings_skeleton_is_recognised() {
    let reason = unfinished_spec_reason(CUDA_BINDINGS_SPEC).unwrap();
    assert!(
        reason.contains("%files lists nothing but %license/%doc"),
        "{reason}"
    );
    assert!(reason.contains("%install has no commands"), "{reason}");
}

#[test]
fn a_finished_spec_is_not_flagged() {
    let spec = "\
Name:           python-foo
%description
Foo
%prep
%autosetup
%build
%pyproject_wheel
%install
%pyproject_install
%python_expand %fdupes %{buildroot}%{$python_sitelib}
%files %{python_files}
%license LICENSE
%{python_sitelib}/foo
%{python_sitelib}/foo-%{version}.dist-info
%changelog
- A TODO in the changelog is history, not a placeholder
";
    assert_eq!(unfinished_spec_reason(spec), None);
}

#[test]
fn the_warning_names_the_package_the_evidence_and_the_way_out() {
    let sizes = vec![
        (
            "python313-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
            11053,
        ),
        (
            "python314-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
            10925,
        ),
    ];
    let reason = unfinished_spec_reason(CUDA_BINDINGS_SPEC).unwrap();
    let msg = empty_package_message("python-cuda-bindings", "home:u:p", &reason, &sizes);
    assert!(
        msg.contains("python-cuda-bindings is built in home:u:p"),
        "{msg}"
    );
    assert!(
        msg.contains("python314-cuda-bindings-13.4.3-2.1.x86_64.rpm: 10925 bytes"),
        "{msg}"
    );
    assert!(msg.contains("--force"), "{msg}");
}

#[test]
fn only_a_succeeded_package_of_this_arch_counts_as_built() {
    let listing: std::collections::HashSet<String> =
        ["python-fastapi-cloud-cli", "python-Foo_Bar", "python-bad"]
            .into_iter()
            .map(String::from)
            .collect();
    let statuses: HashMap<String, BuildStatus> = [
        ("python-fastapi-cloud-cli", BuildStatus::Succeeded),
        ("python-Foo_Bar", BuildStatus::Succeeded),
        ("python-bad", BuildStatus::Unresolvable),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(
        built_in_project(&listing, &statuses, "fastapi-cloud-cli").as_deref(),
        Some("python-fastapi-cloud-cli")
    );
    assert_eq!(
        built_in_project(&listing, &statuses, "foo-bar").as_deref(),
        Some("python-Foo_Bar")
    );
    assert_eq!(built_in_project(&listing, &statuses, "bad"), None);
    assert_eq!(built_in_project(&listing, &statuses, "nothing"), None);
}

#[test]
fn missing_names_collapse_flavors_and_drop_what_is_not_built() {
    let names: Vec<String> = [
        "python313-cuda-bindings",
        "python314-cuda-bindings",
        "python3-cuda-bindings",
        "python3-devel",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(missing_package_names(&names), vec!["cuda-bindings"]);
}

/// Both repositories' results arrive in one document; only the one asked
/// for may count.
const MULTI_REPO: &str = r#"<resultlist state="bfdf69d7aa217ca87031d1aff55605f2">
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-fastapi-cloud-cli" code="unresolvable">
      <details>nothing provides python314-fastar, nothing provides python313-fastar</details>
    </status>
  </result>
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building">
    <status package="python-fastapi-cloud-cli" code="succeeded"/>
  </result>
</resultlist>"#;

#[test]
fn unresolvable_details_come_from_the_requested_arch_only() {
    let details = |arch| {
        unresolvable_details_from_xml(
            MULTI_REPO,
            "openSUSE_Tumbleweed",
            arch,
            "python-fastapi-cloud-cli",
        )
    };
    assert_eq!(
        details("i586").as_deref(),
        Some("nothing provides python314-fastar, nothing provides python313-fastar")
    );
    assert_eq!(details("x86_64"), None);
    assert_eq!(details("aarch64"), None);
}
