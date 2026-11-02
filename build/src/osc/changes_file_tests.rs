use crate::osc::changes::*;
use crate::osc::service::collapse_generated_changes;
use std::fs;
use std::path::Path;

#[test]
fn the_package_changes_file_is_the_one_named_after_the_checkout() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("dbus-1");
    fs::create_dir_all(&dir).unwrap();
    // The subpackage's file sorts first; the package's own is the one to write.
    fs::write(dir.join("dbus-1-devel-doc.changes"), "").unwrap();
    fs::write(dir.join("dbus-1.changes"), "").unwrap();
    assert_eq!(newest_changes_file(&dir), Some(dir.join("dbus-1.changes")));
    assert_eq!(changes_file_name(&dir), "dbus-1.changes");
}

#[test]
fn without_the_package_file_the_first_changes_file_is_used() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("pkg");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("b.changes"), "").unwrap();
    fs::write(dir.join("a.changes"), "").unwrap();
    assert_eq!(newest_changes_file(&dir), Some(dir.join("a.changes")));
    assert_eq!(changes_file_name(&dir), "a.changes");
}

#[test]
fn a_package_without_a_changes_file_is_given_one_named_after_the_checkout() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("elfutils");
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(newest_changes_file(&dir), None);
    assert_eq!(changes_file_name(&dir), "elfutils.changes");
}

#[test]
fn the_failure_text_keeps_what_osc_wrote_on_stdout() {
    // `osc vc` with two changes files: its refusal is on stdout, stderr is empty.
    assert_eq!(
        command_failure_text("Choose one of a.changes b.changes\n", ""),
        "Choose one of a.changes b.changes"
    );
    assert_eq!(command_failure_text("out\n", "err\n"), "err\nout");
    assert_eq!(command_failure_text("", "err"), "err");
    assert_eq!(command_failure_text("  \n", ""), "");
}

fn sep() -> String {
    "-".repeat(67)
}

/// One `.changes` entry: the separator, the header line and `body`.
fn entry(body: &str) -> String {
    format!(
        "{}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n{body}\n",
        sep()
    )
}

/// A checkout with `pkg.changes` as the working copy has it, and `.osc/sources`
/// as OBS holds it.
fn checkout_with(working: &str, obs: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".osc/sources")).unwrap();
    fs::write(dir.path().join(".osc/sources/pkg.changes"), obs).unwrap();
    fs::write(dir.path().join("pkg.changes"), working).unwrap();
    dir
}

/// The baseline a run keeps: `files/` holds what OBS had when the run started.
fn baseline_with(changes: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("files")).unwrap();
    fs::write(dir.path().join("files/pkg.changes"), changes).unwrap();
    dir
}

/// What osc does on a successful `osc ci`: the committed files move into
/// `.osc/sources` (osc's obs_scm `__commit_update_store`), so OBS is what the
/// checkout's working copy was at the commit.
fn osc_ci_succeeds(checkout: &Path) {
    let text = fs::read_to_string(checkout.join("pkg.changes")).unwrap();
    fs::write(checkout.join(".osc/sources/pkg.changes"), text).unwrap();
}

fn read_changes(checkout: &Path) -> String {
    fs::read_to_string(checkout.join("pkg.changes")).unwrap()
}

#[test]
fn a_generated_entry_collapses_against_what_the_run_started_with() {
    let old = entry("- Update to 1.0\n");
    let bullets: String = (0..8).map(|i| format!("  * change {i} (#{i})\n")).collect();
    let generated = entry(&format!("- Update to version 1.3.7:\n{bullets}"));
    let both = format!("{generated}{old}");
    // Round one committed the generated entry, so OBS holds it; the run started
    // with `old` only, and the entry is this run's to collapse.
    let co = checkout_with(&both, &both);
    let base = baseline_with(&old);
    collapse_generated_changes(co.path(), Some(base.path()));
    let out = read_changes(co.path());
    assert!(out.contains("- Update to 1.3.7\n\n"), "{out}");
    assert!(!out.contains("change 3"), "{out}");
    assert!(out.ends_with(&old), "{out}");
}

#[test]
fn without_a_baseline_the_entry_is_compared_with_osc_sources() {
    let old = entry("- Update to 1.0\n");
    let bullets: String = (0..8).map(|i| format!("  * change {i} (#{i})\n")).collect();
    let both = format!(
        "{}{old}",
        entry(&format!("- Update to version 1.3.7:\n{bullets}"))
    );
    // No baseline (first fetch, --local): OBS holds `old`, so the entry collapses.
    let co = checkout_with(&both, &old);
    collapse_generated_changes(co.path(), None);
    assert!(!read_changes(co.path()).contains("change 3"));
    // A baseline directory that is not there is no baseline either.
    let co = checkout_with(&both, &old);
    let missing = co.path().join("no-such-baseline");
    collapse_generated_changes(co.path(), Some(&missing));
    assert!(!read_changes(co.path()).contains("change 3"));
    // And an entry OBS already holds is left alone.
    let co = checkout_with(&both, &both);
    collapse_generated_changes(co.path(), None);
    assert_eq!(read_changes(co.path()), both);
}

#[test]
fn a_long_line_is_wrapped_again_in_a_later_round_against_what_obs_holds() {
    let long1 = "- Add a long description of the first round change that runs past the changelog width of sixty-seven";
    let long2 = "- Add a second long description of a later round change that also runs past the changelog width";
    let round1_entry = entry(long1);
    let co = checkout_with(&round1_entry, &entry("- Update to 1.0\n"));
    wrap_unpublished_changes(co.path());
    let round1 = read_changes(co.path());
    assert!(round1.lines().all(|l| l.chars().count() <= 67), "{round1}");
    // Round one's `osc ci` committed the wrapped entry.
    osc_ci_succeeds(co.path());
    // Round two: the agent adds a bullet; the wrap reads what OBS holds now.
    fs::write(
        co.path().join("pkg.changes"),
        entry(&format!("{long1}\n{long2}")),
    )
    .unwrap();
    wrap_unpublished_changes(co.path());
    let round2 = read_changes(co.path());
    assert!(round2.lines().all(|l| l.chars().count() <= 67), "{round2}");
    // The first bullet wraps as it did when it was committed.
    let round1_bullet = round1.split("\n\n").nth(1).unwrap().trim_end();
    assert!(round2.contains(round1_bullet), "{round2}");
    assert!(round2.contains("second long description"), "{round2}");
}

#[test]
fn an_entry_obs_holds_is_kept_by_a_later_round_for_another_version() {
    let old = entry("- Update to 1.0\n");
    let v2 = entry("- Update to 2.0.0\n");
    let round1 = format!("{v2}{old}");
    // Round one committed 2.0.0; the version moves on to 2.0.1 in round two.
    let co = checkout_with(&round1, &old);
    osc_ci_succeeds(co.path());
    let working = read_changes(co.path());
    // OBS holds 2.0.0: it is not dropped.
    assert!(
        without_unpublished_update_entry(&working, &read_obs(co.path()), "Update to 2.0.1")
            .is_none()
    );
    // osc's copy had not moved on: the 2.0.0 entry is not on OBS and goes.
    let dropped = without_unpublished_update_entry(&working, &old, "Update to 2.0.1").unwrap();
    assert!(!dropped.contains("2.0.0"), "{dropped}");
}

fn read_obs(checkout: &Path) -> String {
    fs::read_to_string(checkout.join(".osc/sources/pkg.changes")).unwrap()
}
