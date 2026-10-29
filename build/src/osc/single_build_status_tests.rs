use super::*;

const REPO: &str = "openSUSE_Tumbleweed";
const ARCH: &str = "x86_64";

fn parse(stdout: &str) -> BuildStatus {
    parse_single_build_status(stdout, "python-foo", REPO, ARCH)
}

#[test]
fn a_plain_status_is_read_from_the_last_column() {
    // Real `osc results -r .. -a .. prj pkg` line: repo, arch, package, status.
    let stdout = "openSUSE_Tumbleweed  x86_64  python-foo  succeeded\n";
    assert_eq!(parse(stdout), BuildStatus::Succeeded);
}

#[test]
fn a_starred_status_is_unknown_not_its_unstarred_meaning() {
    // The real incident this guards against: committing a new revision
    // and immediately watching build status used to report `Succeeded`
    // from the *previous* revision's still-displayed, now-stale result,
    // before OBS had even started rebuilding the new one.
    let stdout = "openSUSE_Tumbleweed  x86_64  python-foo  succeeded*\n";
    assert_eq!(
        parse(stdout),
        BuildStatus::Unknown("stale: succeeded".to_string())
    );
}

#[test]
fn a_verbose_outdated_result_is_stale() {
    let stdout = "openSUSE_Tumbleweed x86_64 python-foo outdated (was: finished: succeeded)\n";
    assert!(matches!(parse(stdout), BuildStatus::Unknown(s) if s.starts_with("stale: ")));
}

#[test]
fn a_bare_finished_takes_its_verdict_from_the_details() {
    let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished: failed\n";
    assert_eq!(parse(stdout), BuildStatus::Failed);
    let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished: succeeded\n";
    assert_eq!(parse(stdout), BuildStatus::Succeeded);
}

#[test]
fn a_finished_without_a_verdict_is_not_terminal() {
    let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished\n";
    let status = parse(stdout);
    assert_eq!(status, BuildStatus::Unknown("finished".to_string()));
    assert!(!status.is_terminal());
}

#[test]
fn verbose_details_after_a_code_are_ignored() {
    let stdout = "openSUSE_Tumbleweed x86_64 python-foo failed: some details\n";
    assert_eq!(parse(stdout), BuildStatus::Failed);
}

#[test]
fn an_unpublished_success_counts_as_succeeded() {
    let stdout = "openSUSE_Tumbleweed x86_64 python-foo succeeded(unpublished)\n";
    assert_eq!(parse(stdout), BuildStatus::Succeeded);
}

#[test]
fn lines_for_other_repos_arches_and_packages_are_skipped() {
    let stdout = "\
openSUSE_Leap x86_64 python-foo failed
openSUSE_Tumbleweed aarch64 python-foo failed
openSUSE_Tumbleweed x86_64 python-foo:flavor failed
openSUSE_Tumbleweed x86_64 python-foo building
";
    assert_eq!(parse(stdout), BuildStatus::Building);
    assert_eq!(
        parse_single_build_status("", "python-foo", REPO, ARCH),
        BuildStatus::Unknown("no results".to_string())
    );
}
