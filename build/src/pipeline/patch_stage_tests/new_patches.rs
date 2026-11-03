//! The rules and the `.changes` name of a patch a RefreshPatches turn adds: the
//! same checks a fix turn's new patch meets.

use super::*;

/// A header that passes the rules; its patch applies in the fake check (`clean`).
const NEW_PATCH_OK: &str = "Subject: Fix the build with gcc 15\n\
Description: gcc 15 rejects the implicit int in foo.c, so the build stops.\n\
Upstream: not yet reported\n\
---\n\
--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n+clean\n";

/// The turn writes `new-fix.patch` with `text` and declares it (with its
/// PATCH comment) in the spec before `%prep`.
fn add_new_patch(checkout: &Path, spec_path: &Path, text: &str) {
    std::fs::write(checkout.join("new-fix.patch"), text).unwrap();
    let spec = std::fs::read_to_string(spec_path).unwrap();
    let spec = spec.replacen(
        "\n%prep",
        "\n# PATCH-FIX-UPSTREAM new-fix.patch -- gcc 15 build\nPatch1: new-fix.patch\n%prep",
        1,
    );
    std::fs::write(spec_path, spec).unwrap();
}

/// A rebase of the failing `b.patch` that also adds `new-fix.patch` as `text`.
fn rebase_and_add(text: &str, changelog: &str) -> Script {
    let (text, changelog) = (text.to_string(), changelog.to_string());
    Box::new(move |checkout, spec_path| {
        std::fs::write(checkout.join("b.patch"), "clean, rebased").unwrap();
        add_new_patch(checkout, spec_path, &text);
        outcome(crate::agent::Action::Edited, &changelog)
    })
}

#[test]
fn a_refresh_turn_that_adds_a_patch_with_its_header_and_a_named_bullet_is_kept() {
    let case = run_accepting(
        &[("b.patch", "old hunks")],
        false,
        10,
        vec![rebase_and_add(
            NEW_PATCH_OK,
            "Add new-fix.patch: gcc 15 build",
        )],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(case.spec.contains("Patch1: new-fix.patch"), "{}", case.spec);
    assert!(
        case.changes.contains("Add new-fix.patch: gcc 15 build"),
        "{}",
        case.changes
    );
}

#[test]
fn a_rebase_turn_that_adds_a_patch_without_its_upstream_is_refused_and_told_why() {
    let no_upstream = NEW_PATCH_OK.replace("Upstream: not yet reported\n", "");
    let case = run_accepting(
        &[("b.patch", "old hunks")],
        false,
        10,
        vec![
            rebase_and_add(&no_upstream, "Add new-fix.patch: gcc 15 build"),
            rebase_and_add(NEW_PATCH_OK, "Add new-fix.patch: gcc 15 build"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(
        case.contexts.len(),
        2,
        "the refused round is followed by one more"
    );
    let told = &case.contexts[1];
    assert!(told.contains("Your last edit was rolled back"), "{told}");
    assert!(told.contains("new-fix.patch"), "{told}");
    assert!(told.contains("Upstream:"), "{told}");
    assert!(case.spec.contains("Patch1: new-fix.patch"), "{}", case.spec);
}

#[test]
fn a_refresh_turn_that_adds_a_patch_its_changelog_does_not_name_is_refused() {
    let case = run_accepting(
        &[("b.patch", "old hunks")],
        false,
        10,
        vec![
            rebase_and_add(NEW_PATCH_OK, "Refresh b.patch"),
            rebase_and_add(NEW_PATCH_OK, "Add new-fix.patch: gcc 15 build"),
        ],
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 2);
    let told = &case.contexts[1];
    assert!(
        told.contains("does not name the new patch new-fix.patch"),
        "{told}"
    );
    assert_eq!(
        bullets(&case.changes, "Refresh b.patch"),
        1,
        "{}",
        case.changes
    );
}
