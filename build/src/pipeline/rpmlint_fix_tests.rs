use super::build_round::{record_rpmlint_logs, revert_warning};
use super::rpmlint_fix::{
    RpmlintAsk, RpmlintGate, RpmlintTurn, load_rpmlint_shown, persist_rpmlint_shown,
    rpmlint_after_turn, rpmlint_ask, rpmlint_key,
};
use super::*;
use crate::pipeline::apply_turn::TurnEffect;
use std::collections::BTreeSet;

const CFFI: &str = "python-cffi-devel.x86_64: E: devel-file-in-non-devel-package (Badness: 50) \
     /usr/lib64/python3.13/site-packages/cffi/_cffi_errors.h";
const ZERO: &str = "python-pywayland.x86_64: E: zero-length /usr/lib64/python3.13/site-packages/pywayland/_ffi/__init__.pyi";

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A gate with attempts and budget left, under the normal (agent) guards.
fn open_gate() -> RpmlintGate {
    RpmlintGate {
        no_agent: false,
        local: false,
        attempts: 0,
        max_attempts: 3,
        spent_usd: 0.0,
        budget_usd: 5.0,
    }
}

fn keys_of(errors: &[String]) -> BTreeSet<String> {
    errors.iter().map(|e| rpmlint_key(e)).collect()
}

#[test]
fn a_green_build_without_errors_is_built_at_once() {
    assert_eq!(
        rpmlint_ask(&open_gate(), &[], &BTreeSet::new()),
        RpmlintAsk::Build
    );
}

#[test]
fn no_agent_or_local_build_never_asks() {
    let errors = lines(&[CFFI]);
    let no_agent = RpmlintGate {
        no_agent: true,
        ..open_gate()
    };
    let local = RpmlintGate {
        local: true,
        ..open_gate()
    };
    assert_eq!(
        rpmlint_ask(&no_agent, &errors, &BTreeSet::new()),
        RpmlintAsk::Build
    );
    assert_eq!(
        rpmlint_ask(&local, &errors, &BTreeSet::new()),
        RpmlintAsk::Build
    );
}

#[test]
fn new_errors_are_asked_about_once_and_only_new_ones_again() {
    let first = lines(&[CFFI]);
    assert_eq!(
        rpmlint_ask(&open_gate(), &first, &BTreeSet::new()),
        RpmlintAsk::Ask(first.clone())
    );
    // The same lines again, after a turn that did not remove them: handled.
    let shown = keys_of(&first);
    assert_eq!(rpmlint_ask(&open_gate(), &first, &shown), RpmlintAsk::Build);
    // A line a fix introduced is asked about; the old one is not repeated.
    let second = lines(&[CFFI, ZERO]);
    assert_eq!(
        rpmlint_ask(&open_gate(), &second, &shown),
        RpmlintAsk::Ask(vec![ZERO.to_string()])
    );
}

#[test]
fn errors_with_no_attempt_or_budget_left_are_shipped_not_asked() {
    let errors = lines(&[ZERO]);
    let used_up = RpmlintGate {
        attempts: 3,
        ..open_gate()
    };
    assert_eq!(
        rpmlint_ask(&used_up, &errors, &BTreeSet::new()),
        RpmlintAsk::Spent
    );
    let spent = RpmlintGate {
        spent_usd: 5.0,
        ..open_gate()
    };
    assert_eq!(
        rpmlint_ask(&spent, &errors, &BTreeSet::new()),
        RpmlintAsk::Spent
    );
    // Already shown: built with no warning from this round.
    assert_eq!(
        rpmlint_ask(&used_up, &errors, &keys_of(&errors)),
        RpmlintAsk::Build
    );
}

#[test]
fn the_turn_effect_decides_rebuild_or_keep() {
    let errors = lines(&[ZERO]);
    assert_eq!(
        rpmlint_after_turn(Ok(TurnEffect::Edited), "python-pywayland", &errors),
        RpmlintTurn::Rebuild
    );
    assert_eq!(
        rpmlint_after_turn(Ok(TurnEffect::SwitchedToWheel), "python-pywayland", &errors),
        RpmlintTurn::Rebuild
    );
    let RpmlintTurn::Keep(text) =
        rpmlint_after_turn(Ok(TurnEffect::NoChange), "python-pywayland", &errors)
    else {
        panic!("a turn that changed nothing keeps the built spec");
    };
    assert!(text.contains("changed nothing"), "{text}");
    assert!(text.contains(ZERO), "{text}");
}

#[test]
fn an_unusable_turn_keeps_the_built_spec_with_a_warning() {
    let errors = lines(&[ZERO]);
    // A turn that needs a dependency not built yet, or whose agent failed.
    let RpmlintTurn::Keep(text) = rpmlint_after_turn(
        Err("it needs cuda-bindings built first".to_string()),
        "python-pywayland",
        &errors,
    ) else {
        panic!("an unusable turn must not rebuild");
    };
    assert!(text.contains("could not be applied"), "{text}");
    assert!(text.contains("cuda-bindings built first"), "{text}");
    assert!(text.contains("the spec that built stands"), "{text}");
    assert!(text.contains(ZERO), "{text}");
}

#[test]
fn a_reverted_rpmlint_edit_names_its_lines() {
    let errors = lines(&[ZERO]);
    let text = revert_warning(
        "python-pywayland",
        &RevertKind::Rpmlint(errors.clone()),
        "BuildFailed",
    );
    assert!(text.contains("shipped with rpmlint errors"), "{text}");
    assert!(text.contains("reverted to the spec that built"), "{text}");
    assert!(text.contains(ZERO), "{text}");
    // The post-build review keeps its own wording.
    let review = revert_warning("python-pywayland", &RevertKind::PostReview, "x");
    assert!(review.contains("post-build review failed"), "{review}");
}

#[test]
fn the_key_ignores_subpackage_badness_arch_and_python_version() {
    let a = "python-cffi-devel.x86_64: E: devel-file-in-non-devel-package (Badness: 50) \
             /usr/lib64/python3.13/site-packages/cffi/_cffi_errors.h";
    let b = "python-cffi.aarch64: E: devel-file-in-non-devel-package (Badness: 10) \
             /usr/lib64/python3.11/site-packages/cffi/_cffi_errors.h";
    assert_eq!(rpmlint_key(a), rpmlint_key(b));
    // Another rule, or another file, is another defect.
    let other_rule = a.replace("devel-file-in-non-devel-package", "zero-length");
    assert_ne!(rpmlint_key(a), rpmlint_key(&other_rule));
    let other_file = a.replace("_cffi_errors.h", "_other.h");
    assert_ne!(rpmlint_key(a), rpmlint_key(&other_file));
    // A line without a path keys on its rule and detail.
    assert_eq!(
        rpmlint_key("pkg.x86_64: E: no-manual-page-dir (Badness: 1)"),
        rpmlint_key("pkg-doc.noarch: E: no-manual-page-dir")
    );
}

#[test]
fn shown_lines_persist_for_their_version_only() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(&root.path().join("state"), "python-pywayland").unwrap();
    let shown = keys_of(&lines(&[ZERO]));
    persist_rpmlint_shown(&ws, "1.0", &shown);
    let prior = PkgState::load(&ws);
    assert_eq!(load_rpmlint_shown(&prior, "1.0"), shown);
    // A later version asks again.
    assert!(load_rpmlint_shown(&prior, "1.1").is_empty());
}

#[test]
fn a_state_file_without_the_fields_loads_with_none_shown() {
    let prior: PkgState = serde_json::from_str("{}").unwrap();
    assert!(load_rpmlint_shown(&prior, "1.0").is_empty());
}

#[test]
fn the_stashed_rpmlint_log_gives_its_error_lines_not_its_warnings() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(&root.path().join("state"), "python-cffi").unwrap();
    let rpms = root.path().join("rpms");
    let stash = rpms.join(".rpmlint");
    std::fs::create_dir_all(&stash).unwrap();
    let error = "python-cffi.x86_64: E: zero-length /usr/lib64/python3.13/site-packages/x.pyi";
    std::fs::write(
        stash.join("python-cffi.log"),
        format!("python-cffi.x86_64: W: spelling-error x\n{error}\n"),
    )
    .unwrap();
    let record = record_rpmlint_logs(&rpms, "python-cffi", "2.0", &ws);
    assert_eq!(record.errors, lines(&[error]));
    assert_eq!(record.warnings.len(), 1, "{:?}", record.warnings);
    // A log with no E: line has no errors, and the stash is taken once.
    std::fs::write(
        stash.join("python-cffi.log"),
        "python-cffi.x86_64: W: spelling-error x\n",
    )
    .unwrap();
    let clean = record_rpmlint_logs(&rpms, "python-cffi", "2.0", &ws);
    assert!(clean.errors.is_empty() && clean.warnings.is_empty());
    assert!(
        record_rpmlint_logs(&rpms, "python-cffi", "2.0", &ws)
            .errors
            .is_empty()
    );
}
