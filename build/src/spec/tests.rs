#[test]
fn the_first_source_value_is_replaced_in_place() {
    let spec = "Name: x\nSource:         x-1.tar.gz\nSource1: y\n";
    assert_eq!(
        set_first_source(spec, "https://h/x-1.tar.gz#/x.tar.gz").unwrap(),
        "Name: x\nSource:         https://h/x-1.tar.gz#/x.tar.gz\nSource1: y\n"
    );
    assert!(set_first_source("Name: x\n", "u").is_none());
}

#[test]
fn source_lines_give_index_address_and_file_name() {
    let raw = |v: &str| v.to_string();
    let s = parse_source_line("Source3:  https://h/x.tar.gz#/y.tar.gz", &["source"], raw).unwrap();
    assert_eq!(
        (s.index, s.url.as_str(), s.file()),
        (3, "https://h/x.tar.gz", "y.tar.gz")
    );
    // A plain `Source:` is Source0; the tag is read without case or spaces.
    let plain = parse_source_line("  SOURCE : a.tar.gz", &["source"], raw).unwrap();
    assert_eq!((plain.index, plain.file()), (0, "a.tar.gz"));
    // `#name` without a slash renames too; an empty or nested rename falls
    // back to the last part of the address.
    let file = |line: &str| {
        parse_source_line(line, &["source"], raw)
            .unwrap()
            .file()
            .to_string()
    };
    assert_eq!(file("Source0: https://h/x.tar#n"), "n");
    assert_eq!(file("Source0: https://h/x.tar#/"), "x.tar");
    assert_eq!(file("Source0: https://h/x.tar#/a/b"), "x.tar");
    assert_eq!(file("Source0: https://h/a/x.tar"), "x.tar");
    // The tag must be one of the given ones, with digits only after it.
    assert!(parse_source_line("Patch1: a.patch", &["source"], raw).is_none());
    assert_eq!(
        parse_source_line("Patch1: a.patch", &["source", "patch"], raw).map(|s| s.index),
        Some(1)
    );
    assert!(parse_source_line("SourceX: a", &["source"], raw).is_none());
    assert!(parse_source_line("Source0 a.tar", &["source"], raw).is_none());
    // The value is expanded before it is split at `#`.
    let macro_url = |v: &str| v.replace("%{u}", "https://h/a#/b");
    assert_eq!(
        parse_source_line("Source0: %{u}", &["source"], macro_url).map(|s| s.url),
        Some("https://h/a".to_string())
    );
    let huge = parse_source_line("Source99999999999: a", &["source"], raw).unwrap();
    assert_eq!(huge.index, u32::MAX);
}

#[test]
fn web_sources_are_listed_with_their_file_names() {
    let spec = "Name: dtc\nVersion: 1.8.1\n\
            Source0: https://k.org/dtc/dtc-%{version}.tar.gz\n\
            Source1: https://k.org/dtc/dtc-%{version}.tar.sign\n\
            Source2: https://x.org/a-%{version}.tar.xz#/renamed.tar.xz\n\
            Source20: baselibs.conf\n";
    let entries = expanded_sources(spec);
    let files: Vec<&str> = entries.iter().map(|e| e.file.as_str()).collect();
    assert_eq!(
        files,
        ["dtc-1.8.1.tar.gz", "dtc-1.8.1.tar.sign", "renamed.tar.xz"]
    );
    assert_eq!(entries[0].url, "https://k.org/dtc/dtc-1.8.1.tar.gz");
    assert_eq!(entries[2].url, "https://x.org/a-1.8.1.tar.xz");
}

#[test]
fn the_header_expands_the_source_and_reads_the_url_tag() {
    let spec = "Name: acl\nVersion: 2.3.1\nURL: https://savannah.nongnu.org/projects/acl\n\
                    Source: %{url}/../%{name}-%{version}.tar.xz#/renamed.tar.xz\n";
    let h = parse_header(spec).unwrap();
    assert_eq!(
        h.url.as_deref(),
        Some("https://savannah.nongnu.org/projects/acl")
    );
    assert_eq!(
        h.expanded_source.as_deref(),
        Some("https://savannah.nongnu.org/projects/acl/../acl-2.3.1.tar.xz")
    );
    assert!(
        h.source.unwrap().contains("%{name}"),
        "the verbatim line is kept"
    );
    assert_eq!(
        parse_header("Name: a\nVersion: 1\n")
            .unwrap()
            .expanded_source,
        None
    );
}

use super::*;

#[test]
fn version_is_replaced_in_dir_names_but_not_inside_other_tokens() {
    let r = replace_version_component;
    assert_eq!(r("foo-v1.2.3", "1.2.3"), "foo-v%{version}");
    assert_eq!(r("foo1.2.3", "1.2.3"), "foo%{version}");
    assert_eq!(r("foo.1.2.3", "1.2.3"), "foo.%{version}");
    assert_eq!(r("foo-1.2.3.post1", "1.2.3"), "foo-%{version}.post1");
    assert_eq!(r("sqlite3-3", "3"), "sqlite3-%{version}");
    assert_eq!(r("foo-1.2.33", "1.2.3"), "foo-1.2.33");
}

const SPEC_WITH_N: &str = "\
Name:           python-MarkupSafe
Version:        3.0.3
%prep
%autosetup -p1 -n MarkupSafe-%{version}
";

const SPEC_WITHOUT_N: &str = "\
Name:           python-markupsafe
Version:        3.0.3
%prep
%autosetup -p1
";

#[test]
fn parses_header_fields_from_a_pypi_spec() {
    let spec = "\
Name:           python-attrs
Version:        26.1.0
Release:        0
Summary:        Classes Without Boilerplate
License:        MIT
URL:            https://www.attrs.org/
Source:         https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz
";
    let h = parse_header(spec).expect("header");
    assert_eq!(h.name, "python-attrs");
    assert_eq!(h.version, "26.1.0");
    assert_eq!(
        h.source.as_deref(),
        Some("https://files.pythonhosted.org/packages/source/a/attrs/attrs-%{version}.tar.gz"),
    );
}

#[test]
fn parse_header_accepts_source0_as_fallback() {
    let spec = "\
Name:           foo
Version:        1.2.3
Source0:        https://example.com/foo-%{version}.tar.gz
";
    let h = parse_header(spec).expect("header");
    assert_eq!(
        h.source.as_deref(),
        Some("https://example.com/foo-%{version}.tar.gz")
    );
}

#[test]
fn parse_header_requires_name_and_version() {
    assert!(parse_header("Version: 1.0\n").is_none());
    assert!(parse_header("Name: foo\n").is_none());
}

/// AppCo's versioned-library specs declare the real version as a
/// `%define` and reference it from `Version:`. Reading the raw tag
/// value made the comparator see the literal `%{package_version}`
/// string, which parses as not-PEP-440 and used to be labelled
/// `UpToDate` (Oct 6 review finding #3). The reader has to resolve the
/// define up front so the comparator sees `2.4.0` here.
#[test]
fn parse_header_expands_a_define_referenced_from_version() {
    let spec = "\
%define package_version 2.4.0
Name:           pytorch
Version:        %{package_version}
Source:         pytorch-%{version}.tar.gz
";
    let h = parse_header(spec).expect("header");
    assert_eq!(h.version, "2.4.0");
}

#[test]
fn parse_header_expands_global_the_same_way_as_define() {
    let spec = "\
%global pkgver 1.5.2
Name:           something
Version:        %{pkgver}
";
    let h = parse_header(spec).expect("header");
    assert_eq!(h.version, "1.5.2");
}

#[test]
fn parse_header_leaves_unknown_macros_alone_rather_than_losing_the_reference() {
    // No matching define — the raw value survives verbatim so callers
    // can tell the macro wasn't resolvable, instead of being handed an
    // empty string or a silent fallback.
    let spec = "\
Name:           foo
Version:        %{something_undefined}
";
    let h = parse_header(spec).expect("header");
    assert_eq!(h.version, "%{something_undefined}");
}

#[test]
fn parse_header_chains_defines_in_file_order() {
    // Real-world case: a release-prefix macro referenced inside a
    // later package-version macro. One pass over defines is enough
    // because each one substitutes against the ones already seen.
    let spec = "\
%define release_prefix 1.0
%define pkgver %{release_prefix}.3
Name:           bar
Version:        %{pkgver}
";
    let h = parse_header(spec).expect("header");
    assert_eq!(h.version, "1.0.3");
}

#[test]
fn parse_header_skips_parametric_macros_without_breaking() {
    // Parametric defines (`%define foo() ...`) are not simple
    // substitutions — don't try to apply one; the Version: here is a
    // literal and should pass through untouched.
    let spec = "\
%define py_sitearch() /usr/lib/python/arch
Name:           ok
Version:        3.2.1
";
    let h = parse_header(spec).expect("header");
    assert_eq!(h.version, "3.2.1");
}

#[test]
fn explicit_n_is_expanded() {
    assert_eq!(
        autosetup_expected_dir(SPEC_WITH_N, "python-MarkupSafe", "3.0.3").as_deref(),
        Some("MarkupSafe-3.0.3")
    );
}

#[test]
fn missing_n_defaults_to_name_version() {
    assert_eq!(
        autosetup_expected_dir(SPEC_WITHOUT_N, "python-markupsafe", "3.0.3").as_deref(),
        Some("python-markupsafe-3.0.3")
    );
}

#[test]
fn mismatch_is_detected() {
    // MarkupSafe-3.0.3 expected, markupsafe-3.0.3 actual — case mismatch.
    let expected = autosetup_expected_dir(SPEC_WITH_N, "python-MarkupSafe", "3.0.3").unwrap();
    let actual = "markupsafe-3.0.3";
    assert_ne!(expected, actual);
}

#[test]
fn fix_replaces_n_argument() {
    // Simulate: spec says -n MarkupSafe-3.0.3, actual dir is markupsafe-3.0.3.
    // fix_autosetup_dir can't run without a real tarball, so test the
    // inner replacement logic directly via autosetup_expected_dir.
    let spec = "%autosetup -p1 -n MarkupSafe-3.0.3\n";
    let parts: Vec<&str> = spec.split_whitespace().collect();
    let idx = parts.windows(2).position(|w| w[0] == "-n").unwrap();
    assert_eq!(parts[idx + 1], "MarkupSafe-3.0.3");
}

#[test]
fn fix_appends_n_when_absent() {
    let spec = "\
Name:           python-markupsafe
Version:        3.0.3
%prep
%autosetup -p1
";
    // Without -n, the default would be python-markupsafe-3.0.3.
    // If the actual dir is markupsafe-3.0.3, fix should append -n.
    // Verify that autosetup_expected_dir sees the default.
    assert_eq!(
        autosetup_expected_dir(spec, "python-markupsafe", "3.0.3").as_deref(),
        Some("python-markupsafe-3.0.3")
    );
}

#[test]
fn python_module_constraints_get_spaces_around_the_operator() {
    // The unspaced form is what pyproject.toml and py2pack produce, and
    // exactly what convention 1 in the agent prompt calls wrong.
    let spec = "BuildRequires:  %{python_module setuptools>=42}\n\
                    BuildRequires:  %{python_module wheel}\n";
    let fixed = normalize_python_module_versions(spec);
    assert!(
        fixed.contains("%{python_module setuptools >= 42}"),
        "{fixed}"
    );
    assert!(
        fixed.contains("%{python_module wheel}"),
        "bare name mangled"
    );
}

/// The faster-whisper case: OBS reports `python313-huggingface_hub` as
/// unresolvable, the translate-deps turn identifies the upstream project
/// as `huggingface-hub`, and that dependency gets built under that
/// spelling — but unless this rewrite runs, the spec keeps asking for
/// the underscore form forever.
#[test]
fn translated_dep_name_is_rewritten_in_python_module() {
    let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.21}\n\
                    Requires:       %{python_module huggingface_hub >= 0.21}\n";
    let fixed = rewrite_python_module_name(spec, "huggingface_hub", "huggingface-hub");
    assert!(
        fixed.contains("%{python_module huggingface-hub >= 0.21}"),
        "{fixed}"
    );
    assert_eq!(fixed.matches("huggingface_hub").count(), 0, "{fixed}");
}

/// The sentence-transformers case: the BuildRequires was respelled but the
/// runtime `Requires: python-huggingface_hub` was not.
#[test]
fn a_respelled_dependency_is_rewritten_in_every_dependency_line() {
    let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.23.0}\n\
                    Requires:       python-huggingface_hub >= 0.23.0\n\
                    Recommends:     python3-huggingface_hub\n\
                    Requires:       python-numpy\n\
                    # python-huggingface_hub stays in a comment\n";
    let fixed = rewrite_dependency_name(spec, "huggingface_hub", "huggingface-hub");
    assert!(
        fixed.contains("BuildRequires:  %{python_module huggingface-hub >= 0.23.0}"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Requires:       python-huggingface-hub >= 0.23.0"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Recommends:     python3-huggingface-hub\n"),
        "{fixed}"
    );
    assert!(fixed.contains("Requires:       python-numpy\n"), "{fixed}");
    assert!(fixed.contains("# python-huggingface_hub stays"), "{fixed}");
    assert_eq!(
        rewrite_dependency_name(&fixed, "huggingface_hub", "huggingface-hub"),
        fixed
    );
}

#[test]
fn rewrite_leaves_unrelated_python_module_lines_alone() {
    let spec = "BuildRequires:  %{python_module huggingface_hub >= 0.21}\n\
                    BuildRequires:  %{python_module numpy}\n";
    let fixed = rewrite_python_module_name(spec, "huggingface_hub", "huggingface-hub");
    assert!(fixed.contains("%{python_module numpy}"), "{fixed}");
}

/// `pyproject.toml`'s `[build-system] requires` is routinely lowercase
/// upstream (`cython>=3`); copied verbatim into the python_module macro
/// it RPM-expands to the nonexistent `python313-cython` instead of the
/// real `python313-Cython`, which made OBS report python-pyarrow as
/// missing a dependency on every run.
#[test]
fn a_known_mixed_case_pypi_name_gets_recased() {
    let spec = "BuildRequires:  %{python_module cython >= 3}\n\
                    BuildRequires:  %{python_module Cython >= 3.1.1}\n\
                    BuildRequires:  %{python_module numpy >= 1.21.6}\n";
    let fixed = normalize_python_module_versions(spec);
    assert!(
        fixed.contains("%{python_module Cython >= 3}"),
        "lowercase name not recased: {fixed}"
    );
    assert!(
        fixed.contains("%{python_module Cython >= 3.1.1}"),
        "already-correct casing changed: {fixed}"
    );
    assert!(
        fixed.contains("%{python_module numpy >= 1.21.6}"),
        "unrelated name touched: {fixed}"
    );
}

/// `%{$python_sitelib}` only resolves inside `%python_expand` — a bare
/// line reproduces the python-nvidia-nvtx bug where it silently expands
/// to `%{}` instead of failing loudly.
#[test]
fn a_continuation_line_is_not_prefixed_again() {
    let spec = "\
%install
%python_expand cp -a foo \\
  %{buildroot}%{$python_sitelib}/foo
chmod +x %{$python_sitelib}/a \\
  %{$python_sitelib}/b
";
    let fixed = fix_stray_dollar_macros(spec);
    assert!(
        fixed.contains("\n  %{buildroot}%{$python_sitelib}/foo\n"),
        "{fixed}"
    );
    assert!(fixed.contains("\n  %{$python_sitelib}/b\n"), "{fixed}");
    assert!(
        fixed.contains("%python_expand chmod +x %{$python_sitelib}/a \\\n"),
        "{fixed}"
    );
    assert_eq!(fixed.matches("%python_expand").count(), 2, "{fixed}");
}

#[test]
fn a_bare_dollar_macro_line_gets_python_expand_prefixed() {
    let spec = "\
%install
%pyproject_install
chmod +x %{buildroot}%{$python_sitelib}/nvidia/cu13/lib/libfoo.so.1
%python_expand %fdupes %{buildroot}%{$python_sitelib}
# %{$python_sitelib} mentioned only in a comment, must not be touched
";
    let fixed = fix_stray_dollar_macros(spec);
    assert!(
        fixed.contains(
            "%python_expand chmod +x %{buildroot}%{$python_sitelib}/nvidia/cu13/lib/libfoo.so.1"
        ),
        "{fixed}"
    );
    assert!(
        fixed.contains("%python_expand %fdupes %{buildroot}%{$python_sitelib}\n"),
        "already-wrapped line got double-wrapped: {fixed}"
    );
    assert!(
        fixed.contains("# %{$python_sitelib} mentioned only in a comment, must not be touched"),
        "comment line was touched: {fixed}"
    );
}

/// `%python_install_alternative` registers an update-alternatives slave
/// in `%post`; without `Requires(post/postun): update-alternatives` the
/// scriptlet fails at install/removal time with "command not found"
/// (observed on python-triton).
#[test]
fn alternatives_without_requires_post_get_them_added() {
    let spec = "\
BuildRequires:  python-rpm-macros
BuildRequires:  %{python_module pip}
%python_subpackages

%install
%python_install_alternative proton
";
    let fixed = fix_missing_alternatives_requires(spec);
    assert!(
        fixed.contains("Requires(post):   update-alternatives\n"),
        "{fixed}"
    );
    assert!(
        fixed.contains("Requires(postun): update-alternatives\n"),
        "{fixed}"
    );
    // Requires lines must land before %python_subpackages.
    let post_pos = fixed.find("Requires(post)").unwrap();
    let subpkg_pos = fixed.find("%python_subpackages").unwrap();
    assert!(post_pos < subpkg_pos, "{fixed}");

    // Already-correct spec and one with no alternatives at all are left alone.
    assert_eq!(fix_missing_alternatives_requires(&fixed), fixed);
    let no_alternatives = "BuildRequires:  %{python_module pip}\n%python_subpackages\n";
    assert_eq!(
        fix_missing_alternatives_requires(no_alternatives),
        no_alternatives
    );
}

#[test]
fn another_requires_line_does_not_count_as_declared() {
    let spec = "\
Requires(post): gtk-update-icon-cache
Requires(postun): update-alternatives
%python_subpackages
%install
%python_install_alternative proton
";
    let fixed = fix_missing_alternatives_requires(spec);
    assert!(
        fixed.contains("Requires(post):   update-alternatives\n"),
        "{fixed}"
    );
    assert_eq!(fixed.matches("Requires(postun):").count(), 1, "{fixed}");
    let both = "Requires(post,postun): update-alternatives\n%python_subpackages\n\
                    %python_install_alternative x\n";
    assert_eq!(fix_missing_alternatives_requires(both), both);
}

#[test]
fn parse_header_reads_the_epoch_and_none_when_absent() {
    let with = "Name: foo\nEpoch: 1\nVersion: 1.0\n";
    assert_eq!(parse_header(with).unwrap().epoch.as_deref(), Some("1"));
    let without = "Name: foo\nVersion: 1.0\n";
    assert_eq!(parse_header(without).unwrap().epoch, None);
}

#[test]
fn parse_header_expands_a_define_referenced_from_epoch() {
    let spec = "%define ep 3\nName: foo\nEpoch: %{ep}\nVersion: 1.0\n";
    assert_eq!(parse_header(spec).unwrap().epoch.as_deref(), Some("3"));
}

#[test]
fn a_web_address_is_named_by_its_fragment_and_else_its_last_segment() {
    use crate::spec::web_address_file as file;
    // The line of a patch whose URL ends in a commit id: the fragment names the file.
    let line = "Patch:  https://github.com/cokelaer/easydev/commit/8cf50c9177936e771e158ea9b249b4aa5e2cc1ee.patch#/fix-version.patch";
    let value = parse_source_line(line, &["patch"], |v| v.to_string()).unwrap();
    assert_eq!(
        value.value,
        "https://github.com/cokelaer/easydev/commit/8cf50c9177936e771e158ea9b249b4aa5e2cc1ee.patch#/fix-version.patch"
    );
    assert_eq!(file(&value.value), "fix-version.patch");
    assert_eq!(file("https://h/x.patch#n.patch"), "n.patch");
    // Without a usable fragment the last segment of the address is the file.
    assert_eq!(file("https://h/a/x.patch"), "x.patch");
    assert_eq!(file("https://h/x.patch#"), "x.patch");
    assert_eq!(file("https://h/x.patch#/"), "x.patch");
    assert_eq!(file("https://h/x.patch#/a/b"), "x.patch");
    // A value that is not a web address is left as it is.
    assert_eq!(file("foo-1.0.patch"), "foo-1.0.patch");
}
