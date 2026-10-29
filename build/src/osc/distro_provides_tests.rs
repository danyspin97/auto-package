
use super::*;

#[test]
fn an_empty_capability_is_never_provided() {
    // Guards against `zypper search --provides --match-exact ""`,
    // which would otherwise shell out and list unrelated packages
    // instead of cleanly reporting "no capability to check".
    assert!(!distro_provides(""));
    assert!(!distro_provides("   "));
}

#[test]
fn trusts_the_plain_released_tree_and_the_base_distro_obs_project() {
    assert!(is_base_distro_repo(
        "http://download.opensuse.org/tumbleweed/repo/oss/"
    ));
    assert!(is_base_distro_repo(
        "http://download.opensuse.org/update/tumbleweed/"
    ));
    assert!(is_base_distro_repo(
        "https://download.opensuse.org/repositories/openSUSE:/Tumbleweed/standard/"
    ));
    assert!(is_base_distro_repo(
        "https://download.opensuse.org/repositories/openSUSE:/Factory/standard/"
    ));
}

#[test]
fn distrusts_vendor_repos_and_arbitrary_obs_sub_projects() {
    assert!(!is_base_distro_repo(
        "https://ftp.gwdg.de/pub/linux/misc/packman/suse/openSUSE_Tumbleweed/"
    ));
    assert!(!is_base_distro_repo(
        "https://packages.cloud.google.com/yum/repos/cloud-sdk-el10-x86_64"
    ));
    assert!(!is_base_distro_repo(
        "https://download.opensuse.org/repositories/home:/someone:/project/openSUSE_Tumbleweed/"
    ));
    assert!(!is_base_distro_repo(
        "https://download.opensuse.org/repositories/openSUSE:/Tools/openSUSE_Tumbleweed"
    ));
}
