use super::add_build_requires;

/// A provider name that carries a macro or shell expansion is refused: the
/// spec is left as it was, and no line is written.
#[test]
fn a_name_with_macro_or_shell_syntax_is_not_written() {
    let spec = "Name: x\nBuildRequires:  a\n";
    for bad in [
        "x%(echo INJECTED)",
        "%{python_module x%(echo INJECTED)}",
        "%{python_module x}\nRequires: pwned",
        "-x",
        "",
        "%{python_module }",
        "x; rm",
        "x`id`",
    ] {
        assert_eq!(add_build_requires(spec, bad), None, "{bad:?}");
    }
}

/// Plain names, with or without the macro and a version constraint, are
/// written.
#[test]
fn a_plain_name_or_constraint_is_written() {
    let spec = "Name: x\nBuildRequires:  a\n";
    for good in [
        "numpy",
        "Cython",
        "python_dateutil",
        "pkg.name+x-1",
        "%{python_module numpy}",
        "%{python_module setuptools >= 42, < 80}",
    ] {
        let out = add_build_requires(spec, good).unwrap_or_else(|| panic!("{good:?}"));
        assert!(out.contains("BuildRequires:  "), "{out}");
    }
}

#[test]
fn the_line_goes_after_the_last_build_requires_once() {
    let spec = "Name: x\nBuildRequires:  a\nBuildRequires:  b\nRequires: c\n";
    let out = add_build_requires(spec, "%{python_module nvidia-nccl-cu13}").unwrap();
    assert_eq!(
        out,
        "Name: x\nBuildRequires:  a\nBuildRequires:  b\nBuildRequires:  %{python_module nvidia-nccl-cu13}\nRequires: c\n"
    );
    assert!(add_build_requires(&out, "%{python_module nvidia-nccl-cu13}").is_none());
    assert!(add_build_requires("Name: x\n", "a").is_none());
}
