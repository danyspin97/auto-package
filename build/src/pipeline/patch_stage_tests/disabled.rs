//! A RefreshPatches turn that disables a patch: refused unless quilt proves it
//! upstream, and then finished as a removal.

use super::*;

/// A turn that edits the spec with `edit` and answers `Edited`, with no
/// per-patch answer.
fn turn(edit: fn(&str) -> String) -> Box<dyn FnMut(&Path, &Path) -> crate::agent::TurnOutcome> {
    Box::new(move |_, spec_path| {
        let spec = std::fs::read_to_string(spec_path).unwrap();
        std::fs::write(spec_path, edit(&spec)).unwrap();
        outcome(crate::agent::Action::Edited, "")
    })
}

fn autosetup_without_patches(spec: &str) -> String {
    spec.replace("%autosetup -p1", "%autosetup -p1 -N")
}

fn patch_under_false_condition(spec: &str) -> String {
    spec.replace(
        "%autosetup -p1",
        "%setup -q\n%if 0\n%patch -P 0 -p1\n%endif",
    )
}

#[test]
fn a_disable_without_proof_is_refused_and_fed_back() {
    if quilt_missing() {
        return;
    }
    let case = run(
        &[("a.patch", FOREIGN_DIFF)],
        false,
        10,
        vec![
            turn(autosetup_without_patches),
            Box::new(|_, _| outcome(crate::agent::Action::NoChange, "")),
        ],
    );
    assert_eq!(case.contexts.len(), 2, "the refusal is fed back once");
    assert!(
        case.contexts[1].contains("disabling a.patch is refused"),
        "{}",
        case.contexts[1]
    );
    needs_human(&case.result, "a.patch");
    assert!(case.spec.contains("%autosetup -p1\n"), "{}", case.spec);
    assert!(!case.spec.contains("-N"), "{}", case.spec);
    assert!(case.spec.contains("Patch0: a.patch"), "{}", case.spec);
    assert_eq!(drop_bullets(&case.changes), 0, "{}", case.changes);
}

#[test]
fn a_disable_quilt_proves_is_finished_as_a_removal() {
    if quilt_missing() {
        return;
    }
    let case = run(
        &[("up.patch", UPSTREAM_DIFF)],
        false,
        10,
        vec![turn(patch_under_false_condition)],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(!case.spec.contains("up.patch"), "{}", case.spec);
    assert!(!case.checkout.join("up.patch").exists());
    assert_eq!(
        bullets(&case.changes, "Drop up.patch: already applied upstream"),
        1,
        "{}",
        case.changes
    );
    assert_eq!(drop_bullets(&case.changes), 1, "{}", case.changes);
}
