use super::snapshot::*;
use super::*;

use super::helpers::*;

/// `btop` as `osc cat Base:System` shows it: a snapshot of a GitHub project.
const BTOP_SPEC: &str = "\
Name:           btop
Version:        1.4.8+git20261025.af521ae
URL:            https://github.com/aristocratos/btop
Source:         btop-%{version}.tar.gz
";

fn btop_with_release(
    release: &'static dyn Fn(&str) -> Result<(String, String), String>,
) -> Lookups<'static> {
    let repo: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(|_: &str| Ok("1.4.8".to_string())));
    Lookups {
        fetch_repo_latest: repo,
        fetch_release_date: release,
        ..unused_lookups()
    }
}

fn btop_spec(_: &str) -> Result<String, ReadError> {
    Ok(BTOP_SPEC.to_string())
}

#[test]
pub(super) fn the_snapshot_day_is_read_from_a_yyyymmdd_or_an_epoch() {
    assert_eq!(
        snapshot_date("1.4.8+git20261025.af521ae").as_deref(),
        Some("2026-10-25")
    );
    assert_eq!(
        snapshot_date("3.11+git.20260906.7d57c2c").as_deref(),
        Some("2026-09-06")
    );
    assert_eq!(
        snapshot_date("0.5.0.git1780765455").as_deref(),
        Some("2026-06-06")
    );
    assert_eq!(
        snapshot_date("0.74+git.1774221329.f4123361").as_deref(),
        Some("2026-03-22")
    );
}

#[test]
pub(super) fn a_snapshot_version_without_a_date_has_no_day() {
    assert_eq!(snapshot_date("1.9.5.3.git+48ab93a"), None);
    assert_eq!(snapshot_date("1.3.git10"), None);
    assert_eq!(snapshot_date("0.5.6~git0.3ca6938"), None);
}

#[test]
pub(super) fn an_epoch_is_turned_into_its_utc_day() {
    assert_eq!(day_of_epoch(0), "1970-01-01");
    assert_eq!(day_of_epoch(951_782_400), "2000-02-29");
    assert_eq!(day_of_epoch(1_770_809_864), "2026-02-11");
}

#[test]
pub(super) fn a_snapshot_with_an_older_release_is_current() {
    let release: &'static dyn Fn(&str) -> Result<(String, String), String> =
        &|_: &str| Ok(("1.4.8".to_string(), "2026-09-01".to_string()));
    let c = classify("btop", &btop_spec, &btop_with_release(release));
    assert!(
        matches!(c.decision, Decision::UpToDate { ref version, .. } if version == "1.4.8+git20261025.af521ae"),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_snapshot_with_a_newer_release_needs_a_maintainer() {
    let release: &'static dyn Fn(&str) -> Result<(String, String), String> =
        &|_: &str| Ok(("1.4.9".to_string(), "2026-11-01".to_string()));
    let c = classify("btop", &btop_spec, &btop_with_release(release));
    let expected = SkipReason::SnapshotBehind {
        release: "1.4.9".into(),
        released: "2026-11-01".into(),
        snapshot: "2026-10-25".into(),
    };
    assert_eq!(c.decision, Decision::Skipped(expected.clone()));
    assert_eq!(
        expected.to_string(),
        "needs a maintainer: release 1.4.9 (2026-11-01) is newer than the snapshot (2026-10-25)"
    );
}

#[test]
pub(super) fn a_snapshot_whose_release_date_is_unknown_stays_a_snapshot_skip() {
    let release: &'static dyn Fn(&str) -> Result<(String, String), String> =
        &|_: &str| Err("no release in the answer".to_string());
    let c = classify("btop", &btop_spec, &btop_with_release(release));
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::Snapshot { .. })),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_snapshot_without_a_date_does_not_ask_for_a_release_date() {
    let spec_text = BTOP_SPEC.replace("1.4.8+git20261025.af521ae", "1.9.5.3.git+48ab93a");
    let spec = |_: &str| Ok(spec_text.clone());
    let release: &'static dyn Fn(&str) -> Result<(String, String), String> =
        &|_: &str| unreachable!("a snapshot with no date is not compared");
    let c = classify("irqbalance", &spec, &btop_with_release(release));
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::Snapshot { .. })),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_snapshot_of_a_package_with_no_forge_is_a_snapshot_skip() {
    let spec_text = "\
Name:           thing
Version:        1.4.8+git20261025.af521ae
Source:         thing-%{version}.tar.gz
";
    let spec = |_: &str| Ok(spec_text.to_string());
    let repology: &'static dyn Fn(&str) -> Result<String, String> = &|_: &str| Ok("1.4.8".into());
    let release: &'static dyn Fn(&str) -> Result<(String, String), String> =
        &|_: &str| unreachable!("no forge repository to date");
    let lookups = Lookups {
        fetch_repology_latest: repology,
        fetch_release_date: release,
        ..unused_lookups()
    };
    let c = classify("thing", &spec, &lookups);
    assert!(
        matches!(c.decision, Decision::Skipped(SkipReason::Snapshot { .. })),
        "{:?}",
        c.decision
    );
}
