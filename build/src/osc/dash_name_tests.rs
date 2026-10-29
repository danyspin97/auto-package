
use super::*;

#[test]
fn names_that_look_like_options_are_refused() {
    assert!(!is_valid_obs_name("--help"));
    assert!(!is_valid_obs_name("-x"));
    assert!(is_valid_obs_name("python-foo"));
    assert!(check_obs_name("package", "-x").is_err());
    assert!(fetch_project_packages("-x", Emit::quiet()).is_err());
    assert!(list_package_files("-x", "p", Emit::quiet()).is_none());
}
