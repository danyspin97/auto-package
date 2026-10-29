use super::add_console_script_alternatives;

const SPEC: &str = "Name: python-x\n%python_subpackages\n\n%install\n%pyproject_install\n\
                        %python_expand %fdupes x\n\n%files %{python_files}\n%license LICENSE\n\
                        %{python_sitelib}/x\n\n%changelog\n";

#[test]
fn scripts_get_clone_scriptlets_and_files_entries() {
    let out = add_console_script_alternatives(SPEC, &["xcli".to_string()]);
    assert!(out.contains("%pyproject_install\n%python_clone -a %{buildroot}%{_bindir}/xcli\n"));
    assert!(
        out.contains("%post\n%python_install_alternative xcli\n"),
        "{out}"
    );
    assert!(
        out.contains("%postun\n%python_uninstall_alternative xcli\n"),
        "{out}"
    );
    assert!(out.contains("%files %{python_files}\n%python_alternative %{_bindir}/xcli\n"));
    assert!(out.contains("Requires(post):   update-alternatives"));
    // Idempotent, and a no-script project is untouched.
    assert_eq!(
        add_console_script_alternatives(&out, &["xcli".to_string()]),
        out
    );
    assert_eq!(add_console_script_alternatives(SPEC, &[]), SPEC);
}
