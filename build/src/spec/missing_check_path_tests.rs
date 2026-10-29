use super::*;
use crate::spec::checks::*;

#[test]
fn only_absent_test_paths_that_are_run_are_reported() {
    let spec = "%check\n%pytest tests/test_configs tests/a.py::t --ignore=tests/gone %{x}/tests/y\n\
                    # tests/commented\npython -m x test/data/\n\n%files\n";
    let have = |p: &str| p == "tests/a.py";
    assert_eq!(
        missing_check_paths(spec, &have),
        vec!["tests/test_configs".to_string(), "test/data".to_string()]
    );
    assert_eq!(missing_check_path_problems(spec, &|_| true).len(), 0);
    assert_eq!(missing_check_path_problems(spec, &have).len(), 1);
}
