use super::*;

use super::helpers::*;

/// `readline` as `osc cat Base:System` shows it: the patch level comes from a
/// script, so the spec knows only the upstream base `8.3`.
fn script_spec(name: &str, base: &str) -> String {
    format!(
        "\
Name:           {name}
Version:        {base}.%(bash %{{_sourcedir}}/get_version_number.sh %{{_sourcedir}})
Source:         {name}-%{{version}}.tar.gz
"
    )
}

fn repology_says(latest: &'static str) -> Lookups<'static> {
    let repology: &'static dyn Fn(&str) -> Result<String, String> =
        Box::leak(Box::new(move |_: &str| Ok(latest.to_string())));
    Lookups {
        fetch_repology_latest: repology,
        ..unused_lookups()
    }
}

#[test]
pub(super) fn a_script_version_whose_base_is_current_is_current() {
    // bash 5.3.%(…) against upstream's 5.3: the patch level is not compared.
    let spec = |_: &str| Ok(script_spec("bash", "5.3"));
    let c = classify("bash", &spec, &repology_says("5.3"));
    assert!(
        matches!(c.decision, Decision::UpToDate { .. }),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_script_version_whose_upstream_is_behind_its_base_is_current() {
    let spec = |_: &str| Ok(script_spec("ncurses", "6.6"));
    let c = classify("ncurses", &spec, &repology_says("6.5"));
    assert!(
        matches!(c.decision, Decision::UpToDate { .. }),
        "{:?}",
        c.decision
    );
}

#[test]
pub(super) fn a_newer_base_for_a_script_version_needs_a_maintainer() {
    let spec = |_: &str| Ok(script_spec("readline", "8.3"));
    let c = classify("readline", &spec, &repology_says("8.4"));
    let expected = SkipReason::PatchLevelScript {
        packaged: "8.3.%(bash %{_sourcedir}/get_version_number.sh %{_sourcedir})".into(),
        known: "8.3".into(),
        latest: "8.4".into(),
    };
    assert_eq!(c.decision, Decision::Skipped(expected.clone()));
    assert!(
        expected
            .to_string()
            .starts_with("needs a maintainer: patch-level script"),
        "{expected}"
    );
}

#[test]
pub(super) fn a_script_version_with_a_shorter_upstream_is_not_judged() {
    // Upstream's "8" does not say whether the base 8.3 is current.
    let spec = |_: &str| Ok(script_spec("readline", "8.3"));
    let c = classify("readline", &spec, &repology_says("8"));
    assert!(
        matches!(
            c.decision,
            Decision::Skipped(SkipReason::ComputedVersion { .. })
        ),
        "{:?}",
        c.decision
    );
}
