
use super::*;

#[test]
fn only_oscs_own_up_to_date_lines_are_benign() {
    assert!(is_benign_up_output("At revision 12.\n"));
    assert!(is_benign_up_output("nothing to update"));
    assert!(!is_benign_up_output(""));
    assert!(!is_benign_up_output("Skipping foo: conflict"));
    assert!(!is_benign_up_output("Error: At revision 3. failed\n"));
    assert!(!is_benign_up_output("At revision 12.\nTraceback: boom"));
}

#[test]
fn get_binaries_reports_an_uncreatable_destination() {
    // A regular file in the path makes `create_dir_all` fail before any
    // `osc` call.
    let file = std::env::temp_dir().join(format!("ap-osc-notdir-{}", std::process::id()));
    std::fs::write(&file, b"x").unwrap();
    let r = get_binaries("p", "pkg", "r", "a", &file.join("rpms"), Emit::quiet());
    std::fs::remove_file(&file).ok();
    assert!(r.is_err());
}

#[test]
fn a_python_multibuild_flavor_counts_as_an_available_package() {
    let pkgs = parse_package_listing("apache-arrow\napache-arrow:python-pyarrow\nPython-Foo\n\n");
    assert!(pkgs.contains("python-pyarrow"));
    assert!(pkgs.contains("apache-arrow:python-pyarrow"));
    assert!(pkgs.contains("apache-arrow"));
    assert!(pkgs.contains("Python-Foo"));
}

#[test]
fn a_non_python_multibuild_flavor_is_not_promoted_to_a_package() {
    let pkgs = parse_package_listing("gcc15:bootstrap\nfoo:test\n");
    assert!(!pkgs.contains("bootstrap"));
    assert!(!pkgs.contains("test"));
}

/// OBS names are case-sensitive: the listing must keep the spelling it
/// has, or the name passed back to osc points at a package that does not
/// exist (`python-apscheduler` for `python-APScheduler`).
#[test]
fn the_listing_keeps_each_package_in_its_own_case() {
    let pkgs = parse_package_listing("python-APScheduler\n");
    assert!(pkgs.contains("python-APScheduler"));
    assert!(!pkgs.contains("python-apscheduler"));
}

#[test]
fn obs_name_returns_the_listed_spelling_for_a_lowercase_guess() {
    let listing = parse_package_listing("python-APScheduler\npython-pydantic_core\n");
    assert_eq!(
        obs_name(&listing, "python-apscheduler"),
        "python-APScheduler"
    );
    // A separator difference between the guess and the listing still finds it.
    assert_eq!(
        obs_name(&listing, "python-pydantic-core"),
        "python-pydantic_core"
    );
}

#[test]
fn obs_name_keeps_the_guess_when_nothing_is_listed() {
    let listing = parse_package_listing("python-APScheduler\n");
    assert_eq!(obs_name(&listing, "python-croniter"), "python-croniter");
    assert_eq!(
        obs_name(&listing, "python-APScheduler"),
        "python-APScheduler"
    );
}

#[test]
fn obs_name_is_stable_when_several_spellings_match() {
    let listing = parse_package_listing("python-Foo\npython-foo\n");
    assert_eq!(obs_name(&listing, "python-FOO"), "python-Foo");
}
