//! The BuildRequires a generated spec writes for `[build-system] requires`.

use super::*;

/// pysmart and backports.functools-lru-cache: a pyproject that names an extra
/// in its build requirements. The spec must carry the bare distribution name
/// (the extra is no capability OBS can provide) and keep the version bound.
#[test]
fn an_extra_in_a_build_requirement_is_not_written() {
    let release = dummy_release("https://example.org", License::Spdx("MIT".to_string()));
    let build_system = BuildSystem {
        requires: vec![
            "setuptools-scm[toml]".to_string(),
            "pydantic-core[x]>=6.2".to_string(),
            "setuptools>=42".to_string(),
        ],
        backend: Some("setuptools.build_meta".to_string()),
    };
    let spec = "Name: pkg\nBuildRequires:  %{python_module pip}\n%changelog\n";
    let out = post_process_with_sdist(spec, &release, &build_system, SdistFacts::default());
    assert!(
        out.contains("BuildRequires:  %{python_module setuptools_scm}\n"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module pydantic-core >= 6.2}\n"),
        "{out}"
    );
    assert!(
        out.contains("BuildRequires:  %{python_module setuptools >= 42}\n"),
        "{out}"
    );
    assert!(!out.contains('['), "{out}");
}
