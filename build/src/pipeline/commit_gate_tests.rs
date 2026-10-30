//! The missing-source decision of the commit gate and the validation recount
//! for an `obs_scm` checkout: an archive OBS makes at build time is not missing;
//! a patch, another archive or a tarball that is not made at build time is.

use super::*;
use crate::pipeline::service_source_tests::OBS_SCM;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// `OBS_SCM` with the `obs_scm` service's `filename` parameter removed.
fn without_filename(service: &str) -> String {
    service.replace(
        "<param name=\"filename\">hyprland-guiutils</param>\n    ",
        "",
    )
}

#[test]
fn the_archive_obs_makes_from_its_name_is_not_missing() {
    let missing = names(&["hyprland-guiutils-0.2.2.tar.xz"]);
    assert!(still_missing(missing, OBS_SCM).is_empty());
    // The `.tar` the `tar` service packs, before any recompression.
    let tar = names(&["hyprland-guiutils-0.2.2.tar"]);
    assert!(still_missing(tar, OBS_SCM).is_empty());
}

#[test]
fn the_archive_takes_the_recompress_suffix_of_the_service() {
    let gz = OBS_SCM.replace(
        "<param name=\"compression\">xz</param>",
        "<param name=\"compression\">gz</param>",
    );
    assert!(still_missing(names(&["hyprland-guiutils-0.2.2.tar.gz"]), &gz).is_empty());
    assert_eq!(
        still_missing(names(&["hyprland-guiutils-0.2.2.tar.xz"]), &gz),
        names(&["hyprland-guiutils-0.2.2.tar.xz"])
    );
}

#[test]
fn an_archive_with_the_wrong_name_is_missing() {
    for name in [
        "hyprland-guiutil-0.2.2.tar.xz",
        "other-0.2.2.tar.xz",
        "hyprland-guiutils-.tar.xz",
        "hyprland-guiutils-0.2.2.tar.gz",
        "xhyprland-guiutils-0.2.2.tar.xz",
    ] {
        assert_eq!(
            still_missing(names(&[name]), OBS_SCM),
            names(&[name]),
            "{name}"
        );
    }
}

#[test]
fn the_default_filename_is_the_url_name_without_the_scm_suffix() {
    let default = without_filename(OBS_SCM);
    assert!(still_missing(names(&["hyprland-guiutils-0.2.2.tar.xz"]), &default).is_empty());
    let other = default.replace(
        "https://github.com/hyprwm/hyprland-guiutils.git",
        "https://github.com/hyprwm/other.git",
    );
    assert_eq!(
        still_missing(names(&["hyprland-guiutils-0.2.2.tar.xz"]), &other),
        names(&["hyprland-guiutils-0.2.2.tar.xz"])
    );
}

#[test]
fn without_a_tar_service_no_archive_is_made_at_build_time() {
    let no_tar = OBS_SCM.replace("  <service mode=\"buildtime\" name=\"tar\"/>\n", "");
    let missing = names(&["hyprland-guiutils-0.2.2.tar.xz"]);
    assert_eq!(still_missing(missing.clone(), &no_tar), missing);
}

#[test]
fn a_missing_patch_is_still_refused_next_to_the_build_time_archive() {
    let missing = names(&["hyprland-guiutils-0.2.2.tar.xz", "fix.patch"]);
    assert_eq!(still_missing(missing, OBS_SCM), names(&["fix.patch"]));
}

#[test]
fn a_manual_vendor_tarball_is_missing_and_a_buildtime_one_is_not() {
    let vendor = "vendor.tar.zst";
    let manual = OBS_SCM.replace(
        "</services>",
        "  <service mode=\"manual\" name=\"cargo_vendor\"/>\n</services>",
    );
    assert_eq!(still_missing(names(&[vendor]), &manual), names(&[vendor]));
    let buildtime = OBS_SCM.replace(
        "<service mode=\"buildtime\" name=\"tar\"/>",
        "<service mode=\"buildtime\" name=\"tar\"/>\n  <service mode=\"buildtime\" name=\"cargo_vendor\"/>",
    );
    assert!(still_missing(names(&[vendor]), &buildtime).is_empty());
    // The `vendor.tar` suffix follows the compression, `zst` by default.
    let xz = buildtime.replace(
        "<service mode=\"buildtime\" name=\"cargo_vendor\"/>",
        "<service mode=\"buildtime\" name=\"cargo_vendor\"><param name=\"compression\">xz</param></service>",
    );
    assert!(still_missing(names(&["vendor.tar.xz"]), &xz).is_empty());
    assert_eq!(still_missing(names(&[vendor]), &xz), names(&[vendor]));
}

#[test]
fn a_tar_scm_checkout_still_refuses_the_same_missing_tarball() {
    let tar_scm = "<services><service name=\"tar_scm\"><param name=\"url\">https://github.com/hyprwm/hyprland-guiutils.git</param></service></services>";
    let missing = names(&["hyprland-guiutils-0.2.2.tar.xz"]);
    assert_eq!(still_missing(missing.clone(), tar_scm), missing);
    // No `_service` at all: nothing is made at build time either.
    assert_eq!(still_missing(missing.clone(), ""), missing);
}

#[test]
fn the_checkout_recount_keeps_the_patch_and_drops_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("_service"), OBS_SCM).unwrap();
    let expanded = "Name: hyprland-guiutils\nVersion: 0.2.2\nSource0: hyprland-guiutils-0.2.2.tar.xz\nPatch0: fix.patch\n";
    assert_eq!(
        missing_at_checkout(expanded, dir.path()),
        names(&["fix.patch"])
    );
    // Without the `_service` the archive is missing too.
    std::fs::remove_file(dir.path().join("_service")).unwrap();
    assert_eq!(
        missing_at_checkout(expanded, dir.path()),
        names(&["hyprland-guiutils-0.2.2.tar.xz", "fix.patch"])
    );
}
