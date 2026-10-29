
use super::*;
use crate::redist::TestMode;

fn dist(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| s.to_string()).collect()
}

const SPEC: &str = "Name: python-x\nBuildRequires:  python-rpm-macros\nBuildRequires:  %{python_module pip}\n%python_subpackages\n\n%description\nx\n";

#[test]
fn a_cuda_wheel_requires_the_cuda_package_with_the_mode_on() {
    let reqs = dist(&["nvidia-cublas>=13.1", "numpy>=2"]);
    {
        let _off = TestMode::set(false);
        let out = add_runtime_dependencies(SPEC, &reqs, "x");
        assert!(
            out.contains("Requires:       python-nvidia-cublas >= 13.1\n"),
            "{out}"
        );
    }
    let _on = TestMode::set(true);
    let deps = runtime_dependencies(&reqs, "x");
    assert_eq!(deps[0].bound, None);
    assert_eq!(deps[1].bound.as_deref(), Some(">= 2"));
    let out = add_runtime_dependencies(SPEC, &reqs, "x");
    assert!(out.contains("Requires:       cuda-cublas\n"), "{out}");
    assert!(out.contains("BuildRequires:  cuda-cublas\n"), "{out}");
    assert!(out.contains("Requires:       python-numpy >= 2\n"), "{out}");
    assert!(!out.contains("python-nvidia"), "{out}");
    // Already there: left alone.
    assert_eq!(add_runtime_dependencies(&out, &reqs, "x"), out);
}

#[test]
fn the_review_agents_runtime_requires_of_a_cuda_wheel_is_rewritten() {
    let spec = "Requires:       %{python_module nvidia-cublas >= 13.1}\n\
                    Requires:       python-nvidia-cuda-runtime >= 13.0\n\
                    Requires:       python-numpy >= 2\n\
                    BuildRequires:  %{python_module nvidia-cublas}\n\
                    Requires:       python-nvidia-cudnn-cu13\n";
    {
        let _off = TestMode::set(false);
        let out = normalize_runtime_python_module(spec);
        assert!(
            out.contains("Requires:       python-nvidia-cublas >= 13.1\n"),
            "{out}"
        );
    }
    let _on = TestMode::set(true);
    let out = normalize_runtime_python_module(spec);
    assert_eq!(
        out,
        "Requires:       cuda-cublas\n\
             Requires:       cuda-cudart\n\
             Requires:       python-numpy >= 2\n\
             BuildRequires:  %{python_module nvidia-cublas}\n\
             Requires:       cuda-cudnn\n"
    );
    assert_eq!(normalize_runtime_python_module(&out), out);
}
