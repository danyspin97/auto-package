//! The `_service` of an `obs_scm` package: which service supplies the source,
//! the revision move, the `Version:` a tag gives at build time, and the
//! checks that keep a checkout from running or refusing that service.

use super::*;
use std::cell::Cell;

pub(super) const OBS_SCM: &str = "<services>\n  <service mode=\"disabled\" name=\"obs_scm\">\n    <param name=\"url\">https://github.com/hyprwm/hyprland-guiutils.git</param>\n    <param name=\"scm\">git</param>\n    <param name=\"revision\">main</param>\n    <param name=\"changesgenerate\">enable</param>\n    <param name=\"filename\">hyprland-guiutils</param>\n    <param name=\"versionformat\">@PARENT_TAG@+@TAG_OFFSET@</param>\n    <param name=\"versionrewrite-pattern\">v(.*)\\+0</param>\n    <param name=\"versionrewrite-replacement\">\\1</param>\n  </service>\n  <service mode=\"buildtime\" name=\"tar\"/>\n  <service mode=\"buildtime\" name=\"recompress\">\n    <param name=\"file\">*.tar</param>\n    <param name=\"compression\">xz</param>\n  </service>\n  <service mode=\"disabled\" name=\"set_version\"/>\n</services>\n";

fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn example_params() -> Vec<(String, String)> {
    params(&[
        ("versionformat", "@PARENT_TAG@+@TAG_OFFSET@"),
        ("versionrewrite-pattern", "v(.*)\\+0"),
        ("versionrewrite-replacement", "\\1"),
    ])
}

#[test]
fn comments_are_blanked_and_the_offsets_kept() {
    let xml = "a<!-- <service name=\"tar_scm\"/> -->\nb";
    let masked = mask_xml_comments(xml);
    assert_eq!(masked.len(), xml.len());
    assert!(masked.starts_with("a") && masked.ends_with("\nb"));
    assert!(!masked.contains("tar_scm"));
    assert!(mask_xml_comments("x<!-- never closed").starts_with('x'));
}

#[test]
fn a_commented_out_tar_scm_does_not_win_over_obs_scm() {
    let svc = format!(
        "<services><!-- <service name=\"tar_scm\"><param name=\"revision\">old</param></service> -->{}",
        OBS_SCM.trim_start_matches("<services>")
    );
    let masked = mask_xml_comments(&svc);
    let (_, kind) = source_service(&masked).unwrap();
    assert_eq!(kind, SourceService::BuildTime);
    let moved = with_service_revision(&svc, "v0.2.2").unwrap();
    assert!(moved.contains("<param name=\"revision\">v0.2.2</param>"));
    assert!(moved.contains("<param name=\"revision\">old</param>"));
}

#[test]
fn the_example_gives_the_tag_without_its_v_and_offset() {
    assert_eq!(
        build_time_version(&example_params(), "v0.2.2").unwrap(),
        "0.2.2"
    );
    assert!(check_build_time_version(&example_params(), "v0.2.2", "0.2.2").is_ok());
    let wrong = check_build_time_version(&example_params(), "v0.2.2", "0.2.3").unwrap_err();
    assert!(
        wrong.contains("`0.2.2`") && wrong.contains("`0.2.3`"),
        "{wrong}"
    );
}

#[test]
fn a_rewrite_that_does_not_give_the_version_is_refused() {
    let no_match = params(&[
        ("versionformat", "@PARENT_TAG@+@TAG_OFFSET@"),
        ("versionrewrite-pattern", "x(.*)"),
        ("versionrewrite-replacement", "\\1"),
    ]);
    let err = check_build_time_version(&no_match, "v0.2.2", "0.2.2").unwrap_err();
    assert!(err.contains("v0.2.2+0"), "{err}");
}

#[test]
fn without_a_rewrite_only_a_leading_v_is_dropped() {
    assert_eq!(build_time_version(&[], "v0.2.2").unwrap(), "0.2.2");
    assert_eq!(build_time_version(&[], "0.2.2").unwrap(), "0.2.2");
    let plus_zero = params(&[("versionformat", "@PARENT_TAG@+@TAG_OFFSET@")]);
    assert!(check_build_time_version(&plus_zero, "v0.2.2", "0.2.2").is_err());
}

#[test]
fn a_macro_or_prefix_this_cannot_follow_is_refused() {
    let commit = params(&[("versionformat", "@PARENT_TAG@.@CI_SHORT_HASH@")]);
    assert!(build_time_version(&commit, "v1").is_err());
    let prefix = params(&[("versionprefix", "v")]);
    assert!(build_time_version(&prefix, "v1").is_err());
    let bad = params(&[("versionrewrite-pattern", "(")]);
    assert!(build_time_version(&bad, "v1").is_err());
}

#[test]
fn osc_group_references_become_regex_groups() {
    assert_eq!(regex_replacement("\\1-$x"), "${1}-$$x");
}

#[test]
fn a_plan_without_a_revision_is_refused_before_the_tag_lookup() {
    let asked = Cell::new(false);
    let no_revision = "<services><service mode=\"disabled\" name=\"obs_scm\"><param name=\"url\">u</param></service></services>";
    let res = plan_service_carry(no_revision, "0.2.2", || {
        asked.set(true);
        Ok("v0.2.2".into())
    });
    assert!(res.is_err());
    assert!(!asked.get());
}

#[test]
fn a_manual_vendoring_service_is_refused_before_the_tag_lookup() {
    let asked = Cell::new(false);
    let svc = OBS_SCM.replace(
        "<service mode=\"buildtime\" name=\"tar\"/>",
        "<service mode=\"buildtime\" name=\"tar\"/>\n  <service mode=\"manual\" name=\"cargo_vendor\"/>",
    );
    let err = plan_service_carry(&svc, "0.2.2", || {
        asked.set(true);
        Ok("v0.2.2".into())
    })
    .err()
    .unwrap();
    assert!(err.contains("cargo_vendor"), "{err}");
    assert!(!asked.get());
}

#[test]
fn a_no_tag_refusal_is_the_lookups_own() {
    let err = plan_service_carry(OBS_SCM, "0.2.2", || {
        Err("hyprland-guiutils lists no tag for 0.2.2".to_string())
    })
    .err()
    .unwrap();
    assert_eq!(err, "hyprland-guiutils lists no tag for 0.2.2");
}

#[test]
fn the_plan_moves_the_revision_of_an_obs_scm_service() {
    let plan = plan_service_carry(OBS_SCM, "0.2.2", || Ok("v0.2.2".into())).unwrap();
    assert_eq!(plan.kind, SourceService::BuildTime);
    assert_eq!(
        plan.moved,
        OBS_SCM.replace(">main</param>", ">v0.2.2</param>")
    );
    // A version the tag does not give at build time is refused, nothing moved.
    let wrong = plan_service_carry(OBS_SCM, "0.2.3", || Ok("v0.2.2".into()));
    assert!(wrong.is_err());
}

#[test]
fn a_tar_scm_service_is_fetched_and_not_checked_for_a_version() {
    let tar = "<services><service name=\"tar_scm\"><param name=\"revision\">v1.0</param></service></services>";
    let plan = plan_service_carry(tar, "1.1", || Ok("v1.1".into())).unwrap();
    assert_eq!(plan.kind, SourceService::Fetched);
    assert!(plan.moved.contains("<param name=\"revision\">v1.1</param>"));
}

#[test]
fn the_obs_scm_files_are_written_together_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let service = dir.path().join("_service");
    let spec = dir.path().join("pkg.spec");
    std::fs::write(&service, OBS_SCM).unwrap();
    std::fs::write(&spec, "Version: 0.2.1\n").unwrap();
    let moved = OBS_SCM.replace(">main</param>", ">v0.2.2</param>");
    carry_forward_obs_scm_spec(&service, OBS_SCM, &moved, &spec, "Version: 0.2.2\n").unwrap();
    assert_eq!(std::fs::read_to_string(&service).unwrap(), moved);
    assert_eq!(std::fs::read_to_string(&spec).unwrap(), "Version: 0.2.2\n");
    assert!(!dir.path().join("hyprland-guiutils-0.2.2.tar.xz").exists());

    // A spec that cannot be written puts `_service` back as it was.
    let missing_dir = dir.path().join("gone").join("pkg.spec");
    assert!(carry_forward_obs_scm_spec(&service, &moved, OBS_SCM, &missing_dir, "x").is_err());
    assert_eq!(std::fs::read_to_string(&service).unwrap(), moved);
}

#[test]
fn the_checkout_says_whether_obs_makes_the_source() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!source_made_at_build_time(dir.path()));
    std::fs::write(dir.path().join("_service"), OBS_SCM).unwrap();
    assert!(source_made_at_build_time(dir.path()));
    let tar =
        "<services><service name=\"tar_scm\"><param name=\"url\">u</param></service></services>";
    std::fs::write(dir.path().join("_service"), tar).unwrap();
    assert!(!source_made_at_build_time(dir.path()));
    let commented = format!("<services><!-- {OBS_SCM} --></services>");
    std::fs::write(dir.path().join("_service"), commented).unwrap();
    assert!(!source_made_at_build_time(dir.path()));
}

#[test]
fn the_archive_obs_makes_is_not_a_missing_file() {
    let names = vec![
        "hyprland-guiutils-0.2.2.tar.xz".to_string(),
        "fix.patch".to_string(),
    ];
    assert_eq!(still_missing(names, OBS_SCM), ["fix.patch".to_string()]);
}

#[test]
fn the_patches_of_an_obs_scm_source_are_said_not_to_be_checked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("_service"), OBS_SCM).unwrap();
    std::fs::write(dir.path().join("fix.patch"), "--- a/a\n+++ b/a\n").unwrap();
    let spec_path = dir.path().join("hyprland-guiutils.spec");
    let spec = "Name: hyprland-guiutils\nVersion: 0.2.2\nSource0: %{name}-%{version}.tar.xz\nPatch0: fix.patch\n%prep\n%autosetup -p1\n";
    let check = try_patches(&spec_path, spec).unwrap();
    assert!(check.failed.is_empty());
    assert!(check.unchecked.unwrap().contains("OBS makes the source"));
}

#[test]
fn an_obs_scm_checkout_runs_no_service_at_validation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("_service"), OBS_SCM).unwrap();
    let spec_path = dir.path().join("pkg.spec");
    std::fs::write(&spec_path, "Version: 0.2.2\n").unwrap();
    // `osc` would refuse the `tar` service: nothing is run, so nothing fails.
    assert!(
        keep_version::run_service_keeping_version(
            dir.path(),
            "test-pkg",
            &spec_path,
            Emit::quiet()
        )
        .is_ok()
    );
}

#[test]
fn a_url_cargo_vendor_fetches_and_a_buildtime_one_does_not_block_obs_scm() {
    let url = "<services><service mode=\"manual\" name=\"cargo_vendor\"><param name=\"url\">https://github.com/o/r.git</param><param name=\"revision\">v1</param></service></services>";
    assert_eq!(
        source_service(&mask_xml_comments(url)).unwrap().1,
        SourceService::Fetched
    );
    let buildtime = OBS_SCM.replace(
        "<service mode=\"buildtime\" name=\"tar\"/>",
        "<service mode=\"buildtime\" name=\"tar\"/>\n  <service mode=\"buildtime\" name=\"cargo_vendor\"/>",
    );
    assert!(refuse_manual_vendoring(&mask_xml_comments(&buildtime)).is_ok());
    assert!(plan_service_carry(&buildtime, "0.2.2", || Ok("v0.2.2".into())).is_ok());
}
