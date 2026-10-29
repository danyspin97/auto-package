
use super::*;

fn files(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_namespace_distribution_lists_its_real_directory() {
    let f = files(&[
        "otel_flask-0.66b1/src/opentelemetry/instrumentation/flask/__init__.py",
        "otel_flask-0.66b1/src/opentelemetry/instrumentation/flask/package.py",
        "otel_flask-0.66b1/tests/test_flask.py",
        "otel_flask-0.66b1/pyproject.toml",
    ]);
    let entries = sdist_files_entries(&f, "otel_flask-0.66b1", None).unwrap();
    assert_eq!(
        entries,
        [
            "%dir @SITE@/opentelemetry",
            "%dir @SITE@/opentelemetry/instrumentation",
            "@SITE@/opentelemetry/instrumentation/flask/"
        ]
    );
    let spec = "%files %{python_files}\n%license LICENSE\n%{python_sitelib}/opentelemetry_instrumentation_flask/\n%{python_sitelib}/opentelemetry_instrumentation_flask-%{version}.dist-info/\n\n%changelog\n";
    let out = apply_install_entries(spec, &entries);
    assert_eq!(
        out,
        "%files %{python_files}\n%license LICENSE\n%dir %{python_sitelib}/opentelemetry\n%dir %{python_sitelib}/opentelemetry/instrumentation\n%{python_sitelib}/opentelemetry/instrumentation/flask/\n%{python_sitelib}/opentelemetry_instrumentation_flask-%{version}.dist-info/\n\n%changelog\n"
    );
}

#[test]
fn a_different_import_name_replaces_the_pypi_guess_and_a_right_guess_is_kept() {
    let f = files(&[
        "python-docx-1.2.0/src/docx/__init__.py",
        "python-docx-1.2.0/src/docx/x.py",
        "python-docx-1.2.0/tests/t.py",
    ]);
    let entries = sdist_files_entries(&f, "python-docx-1.2.0", None).unwrap();
    assert_eq!(entries, ["@SITE@/docx/"]);
    let wrong = "%files %{python_files}\n%{python_sitelib}/python_docx/\n";
    assert_eq!(
        apply_install_entries(wrong, &entries),
        "%files %{python_files}\n%{python_sitelib}/docx/\n"
    );
    let right = "%files %{python_files}\n%{python_sitearch}/docx/\n";
    assert_eq!(apply_install_entries(right, &entries), right);
}

#[test]
fn an_ambiguous_layout_is_left_to_the_review() {
    // a loose module, or two import trees
    let module = files(&["x-1/six.py", "x-1/tests/t.py"]);
    assert_eq!(sdist_files_entries(&module, "x-1", None), None);
    let two = files(&["x-1/a/__init__.py", "x-1/b/__init__.py"]);
    assert_eq!(sdist_files_entries(&two, "x-1", None), None);
}

#[test]
fn a_traversing_member_is_not_listed_blind() {
    // `pkg/../../x.py` would own `%{sitedir}/pkg/..` and reach outside it
    let escape = files(&["x-1/pkg/__init__.py", "x-1/pkg/../../x.py"]);
    assert_eq!(sdist_files_entries(&escape, "x-1", None), None);
    let dot = files(&["x-1/pkg/__init__.py", "x-1/pkg/./a.py"]);
    assert_eq!(sdist_files_entries(&dot, "x-1", None), None);
    let empty = files(&["x-1/pkg/__init__.py", "x-1/pkg//a.py"]);
    assert_eq!(sdist_files_entries(&empty, "x-1", None), None);
}
