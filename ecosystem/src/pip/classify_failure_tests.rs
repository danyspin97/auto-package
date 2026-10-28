use super::*;

#[test]
fn skipped_for_requires_python_is_not_reported_as_missing() {
    let stderr = "ERROR: Ignored the following versions that require a different python version: 0.8.12 Requires-Python >=3.11,<3.13.0a1; 0.9.0 Requires-Python >=3.11,<3.13.0a1\nERROR: Could not find a version that satisfies the requirement open-webui (from versions: none)\nERROR: No matching distribution found for open-webui\n";
    match classify_failure("open-webui", 1, stderr.to_string(), false) {
        Error::IncompatiblePython { name, example } => {
            assert_eq!(name, "open-webui");
            assert_eq!(example, "0.8.12 Requires-Python >=3.11,<3.13.0a1");
        }
        e => panic!("wrong error: {e}"),
    }
}

#[test]
fn a_truly_missing_project_is_not_found() {
    let stderr = "ERROR: Could not find a version that satisfies the requirement nope (from versions: none)\nERROR: No matching distribution found for nope\n";
    assert!(matches!(
        classify_failure("nope", 1, stderr.to_string(), false),
        Error::NotFound(_)
    ));
}
