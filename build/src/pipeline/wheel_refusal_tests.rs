//! A wheel switch that would drop a security patch is refused before it writes
//! anything, with an error that says the package needs a maintainer.

use super::*;
use crate::pipeline::removal_proof::*;

const SPEC: &str = "Name: python-foo\nVersion: 1.0\nRelease: 0\nSummary: x\nLicense: MIT\n\
                    Source0: foo-1.0.tar.gz\nPatch0: PATCH\n%description\nx\n";
const WHY: &str = "no compiler for the extension in this build environment: error: gcc not found";

struct Case {
    root: tempfile::TempDir,
    config: PipelineConfig,
    checkout: PathBuf,
    spec_path: PathBuf,
    before: String,
    step: StepDir,
}

/// A checkout whose spec declares `patch`, with `disk` as its content.
fn case(patch: &str, disk: &str) -> Case {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("proj").join("python-foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let mut config = super::hygiene_tests::config("python-foo", &root.path().join("state"));
    config.rpm_name = "python-foo".into();
    let before = SPEC.replace("PATCH", patch);
    let spec_path = checkout.join("python-foo.spec");
    std::fs::write(&spec_path, &before).unwrap();
    std::fs::write(checkout.join(patch), disk).unwrap();
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    Case {
        root,
        config,
        checkout,
        spec_path,
        before,
        step,
    }
}

/// PyPI's answer for a release with one pure wheel: the switch has a wheel to
/// pick (its download is never reached by a refused switch).
const WHEEL_JSON: &str = r#"{"info":{"name":"foo","version":"1.0"},"urls":[
  {"digests":{"md5":"aaa","sha256":"0123456789abcdef"},"filename":"foo-1.0-py3-none-any.whl",
   "packagetype":"bdist_wheel","size":10,"url":"https://files.pythonhosted.org/foo-1.0-py3-none-any.whl",
   "yanked":false}]}"#;

/// PyPI's answer for a release with no wheel at all.
const NO_WHEEL_JSON: &str = r#"{"info":{"name":"foo","version":"1.0"},"urls":[]}"#;

/// The wheel switch of `c` with PyPI answering `json`, and whether PyPI was asked.
fn switch(
    c: &Case,
    snapshot: Option<&CheckoutSnapshot>,
    json: &str,
) -> (Result<TurnEffect, TurnError>, bool) {
    let asked = std::cell::Cell::new(false);
    let lookup = |_: &PipelineConfig| {
        asked.set(true);
        Ok::<String, String>(json.to_string())
    };
    let (mut sdist, mut from_repo) = (String::new(), false);
    let result = apply_turn_and_reset_with(
        ApplyTurnCtx {
            config: &c.config,
            checkout_dir: &c.checkout,
            spec_path: &c.spec_path,
            step: &c.step,
            before: &c.before,
            rpm_name: "python-foo",
            label: "Review",
            attempt: 1,
            notes_path: c.root.path().join("notes.md"),
            snapshot,
            kept: &[],
            staged_out: None,
            rejected_out: None,
        },
        &pipeline_wheel_outcome(WHY),
        &ap_core::sink::NULL_REPORTER,
        &mut sdist,
        &mut from_repo,
        &lookup,
    );
    (result, asked.get())
}

fn refused(result: Result<TurnEffect, TurnError>) -> TurnError {
    let e = result.expect_err("the switch is refused");
    assert_eq!(e.kind, FailureKind::NeedsMaintainer, "{e}");
    assert!(e.ends_package());
    e
}

/// The names in the checkout and in the package's state directory.
fn files(c: &Case) -> Vec<String> {
    let ws = Workspace::open(&c.config.state_root, "python-foo").unwrap();
    let mut names: Vec<String> = [c.checkout.clone(), ws.dir().to_path_buf()]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap())
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_refusal_leaves_nothing_behind_with_or_without_a_snapshot() {
    for with_snapshot in [false, true] {
        let c = case("fix-CVE-2024-1234.patch", "--- a/f\n+++ b/f\n");
        let snapshot = CheckoutSnapshot::take(&c.checkout);
        let listed = files(&c);
        // The turn's own edit of the spec is rolled back too.
        std::fs::write(&c.spec_path, "Name: python-foo\nVersion: 2.0\n").unwrap();
        let (result, asked) = switch(&c, with_snapshot.then_some(&snapshot), WHEEL_JSON);
        let e = refused(result);
        assert!(e.reason.contains("fix-CVE-2024-1234.patch"), "{e}");
        // A wheel was found, and only then was the switch refused.
        assert!(asked);
        // No wheel, rpmlintrc, wheel-origin.txt or .pre-wheel-switch.
        assert_eq!(files(&c), listed);
        assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), c.before);
        assert!(c.checkout.join("fix-CVE-2024-1234.patch").exists());
    }
}

#[test]
fn with_no_usable_wheel_the_switch_is_not_refused_and_keeps_the_redirect() {
    let c = case("fix-CVE-2024-1234.patch", "--- a/f\n+++ b/f\n");
    let listed = files(&c);
    let (result, asked) = switch(&c, None, NO_WHEEL_JSON);
    // PyPI is asked first; with no wheel the turn is told to write the spec
    // itself (the removal gate judges that edit), and nothing is written.
    assert!(asked);
    assert_eq!(result.unwrap(), TurnEffect::NoChange);
    assert_eq!(files(&c), listed);
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), c.before);
}

#[test]
fn a_glued_bsc_name_is_a_security_patch_and_boost_is_not() {
    let drops = |patch: &str| {
        let c = case(patch, "--- a/f\n+++ b/f\n");
        wheel_switch_security_drops(&c.checkout, &c.before, None)
    };
    assert_eq!(drops("fix-bsc1234.patch"), ["fix-bsc1234.patch"]);
    assert!(drops("boost-fix.patch").is_empty());
    let c = case("fix-bsc1234.patch", "--- a/f\n+++ b/f\n");
    refused(switch(&c, None, WHEEL_JSON).0);
}

#[test]
fn the_header_is_read_from_the_snapshot_in_preference_to_the_disk() {
    let cve = "Subject: fix\n\nFixes CVE-2024-1234.\n--- a/f\n+++ b/f\n";
    let plain = "Subject: fix\n\n--- a/f\n+++ b/f\n";
    // The snapshot has the CVE; the turn rewrote the patch on disk without it.
    let c = case("fix-foo.patch", cve);
    let snapshot = CheckoutSnapshot::take(&c.checkout);
    std::fs::write(c.checkout.join("fix-foo.patch"), plain).unwrap();
    assert_eq!(
        wheel_switch_security_drops(&c.checkout, &c.before, Some(&snapshot)),
        ["fix-foo.patch"]
    );
    refused(switch(&c, Some(&snapshot), WHEEL_JSON).0);
    // The snapshot has no CVE; the disk, written by the turn, does: not judged.
    let c = case("fix-foo.patch", plain);
    let snapshot = CheckoutSnapshot::take(&c.checkout);
    std::fs::write(c.checkout.join("fix-foo.patch"), cve).unwrap();
    assert!(wheel_switch_security_drops(&c.checkout, &c.before, Some(&snapshot)).is_empty());
}

#[test]
fn the_per_flavor_switch_is_refused_before_it_writes_anything() {
    let mut c = case("fix-CVE-2024-1234.patch", "--- a/f\n+++ b/f\n");
    c.config.python_flavors = std::sync::Arc::new(
        serde_json::from_str(r#"{"flavors":["python313","python314"]}"#).unwrap(),
    );
    let json = r#"{"info":{"name":"foo","version":"1.0"},"urls":[
      {"digests":{"sha256":"01"},"filename":"foo-1.0-cp313-cp313-manylinux_2_17_x86_64.whl",
       "packagetype":"bdist_wheel","size":1,"url":"https://x/a.whl"},
      {"digests":{"sha256":"02"},"filename":"foo-1.0-cp314-cp314-manylinux_2_17_x86_64.whl",
       "packagetype":"bdist_wheel","size":1,"url":"https://x/b.whl"}]}"#;
    let listed = files(&c);
    let result = crate::pipeline::wheel_spec::generate_multi_flavor_wheel_spec(
        &c.config,
        json,
        &c.checkout,
        &c.spec_path,
        &c.step,
        false,
        &|| Err("refused for the test".to_string()),
    );
    // Wheels were picked for both flavors, so the guard ran, and with it
    // refusing nothing was downloaded or written.
    assert!(matches!(result, Some(Err(GenSpecError::Refused(m))) if m == "refused for the test"));
    assert_eq!(files(&c), listed);
    assert_eq!(std::fs::read_to_string(&c.spec_path).unwrap(), c.before);
}
