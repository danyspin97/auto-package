//! A spent ladder's failure: the short reason, the full detail, the action.

use super::*;
use crate::pipeline::ladder_end::*;
use crate::pipeline::ladder_recap::Recap;

const JAY_ZERO: &str = "The cargo_vendor service cannot run because the host's rustup has no \
    default toolchain. Nothing in the checkout can fix that.";

fn no_change(why: &str) -> crate::agent::TurnOutcome {
    scripted_outcome(crate::agent::Action::NoChange, why)
}

fn jay_rounds() -> Vec<crate::agent::TurnOutcome> {
    vec![
        no_change("The only missing source is Source1, vendor.tar.zst, which the service makes."),
        no_change("The service has to run in manual mode and the host cannot run it."),
        no_change(
            "Every service runs in manual mode: convention 77 (section 92) forbids vendoring \
             by hand, so the checkout cannot be changed.",
        ),
    ]
}

fn failed(case: &Case) -> (&str, &FailureKind, &ap_core::failure_note::FailureNote) {
    match &case.result {
        Err(PipelineResult::Failed {
            reason,
            kind,
            note: Some(note),
            ..
        }) => (reason, kind, note),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_jay_scenario_ends_with_one_short_line_and_keeps_the_full_text() {
    let lookups = std::cell::Cell::new(0);
    let case = run_case(
        "jay-compositor",
        false,
        &[],
        blocked(JAY_ZERO),
        jay_rounds(),
        &lookups,
        None,
    );
    let (reason, kind, note) = failed(&case);
    assert_eq!(*kind, FailureKind::Exhausted);
    assert!(reason.chars().count() <= 400, "{reason}");
    assert!(!reason.contains('\n'), "{reason}");
    assert_eq!(
        reason,
        "jay-compositor could not be resolved: The cargo_vendor service cannot run because the \
         host's rustup has no default toolchain. 3 resolution rounds did not fix it."
    );
    assert!(
        reason.starts_with(
            "jay-compositor could not be resolved: The cargo_vendor service cannot run because"
        ),
        "{reason}"
    );
    for gone in [
        "convention 77",
        "section 92",
        "round ",
        "answered",
        "UNRESOLVED",
    ] {
        assert!(!reason.contains(gone), "{gone}: {reason}");
    }
    // The note keeps the old text, raw, and the way to run again.
    let detail = note.detail.as_deref().unwrap();
    assert!(detail.starts_with(LADDER_FAILED_PREFIX), "{detail}");
    assert!(detail.contains("convention 77 (section 92)"), "{detail}");
    assert!(detail.contains("round 1 answered no_change"), "{detail}");
    assert_eq!(
        note.action.as_deref(),
        Some("after fixing the cause, run: auto-package build jay-compositor --force")
    );
    // The journal keeps the numbered list.
    assert!(
        case.notes.contains("\n4. round 3 answered no_change"),
        "{}",
        case.notes
    );
    // The memo has the full text, the short recap and no action of its own.
    let ws = Workspace::open(&case.state_root, "jay-compositor").unwrap();
    let state = PkgState::load(&ws);
    assert!(state.ladder_reason.unwrap().contains("convention 77"));
    let summary = state.ladder_recap.ladder_summary.unwrap();
    assert!(reason.ends_with(&summary), "{summary}");
    assert_eq!(state.ladder_recap.ladder_action, None);
}

#[test]
fn a_resolver_summary_and_action_are_the_recap_and_the_first_step() {
    let lookups = std::cell::Cell::new(0);
    let mut last = jay_rounds().pop().unwrap();
    last.user_summary = Some("The host has no default Rust toolchain.".into());
    last.user_action = Some("Run rustup default stable.".into());
    let mut rounds = jay_rounds();
    rounds[2] = last;
    let case = run_case(
        "jay-compositor",
        false,
        &[],
        blocked(JAY_ZERO),
        rounds,
        &lookups,
        None,
    );
    let (reason, _, note) = failed(&case);
    assert_eq!(
        reason,
        "jay-compositor could not be resolved: The host has no default Rust toolchain."
    );
    assert_eq!(
        note.action.as_deref(),
        Some(
            "Run rustup default stable; after fixing the cause, run: auto-package build \
             jay-compositor --force"
        )
    );
    // Each round's own words go to the journal; the detail is still the history.
    assert!(
        case.notes
            .contains("round 3 user_summary: The host has no default Rust toolchain."),
        "{}",
        case.notes
    );
    assert!(
        case.notes.contains("round 3 user_action: Run rustup"),
        "{}",
        case.notes
    );
    assert!(note.detail.as_deref().unwrap().contains("round 1 answered"));
    let ws = Workspace::open(&case.state_root, "jay-compositor").unwrap();
    let state = PkgState::load(&ws);
    assert_eq!(
        state.ladder_recap.ladder_summary.as_deref(),
        Some("The host has no default Rust toolchain.")
    );
    assert_eq!(
        state.ladder_recap.ladder_action.as_deref(),
        Some("Run rustup default stable.")
    );
}

fn body(headline: &str, lines: &[&str], action: Option<&str>) -> Recap {
    Recap {
        headline: headline.into(),
        lines: lines.iter().map(|l| l.to_string()).collect(),
        action: action.map(str::to_string),
    }
}

#[test]
fn the_rerun_hint_depends_on_how_the_ladder_ended() {
    let hint = "after fixing the cause, run: auto-package build foo --force";
    let plain = body("Body.", &[], None);
    let (reason, action) = short_failure("python-foo", Some("foo"), &plain, 0, &Ending::Remembered);
    assert_eq!(reason, "python-foo could not be resolved: Body.");
    assert_eq!(action.as_deref(), Some(hint));
    // A step of its own comes first.
    let stepped = body("Body.", &[], Some("Do x."));
    let (_, action) = short_failure("python-foo", Some("foo"), &stepped, 0, &Ending::Remembered);
    assert_eq!(action.as_deref(), Some(format!("Do x; {hint}").as_str()));
    // A request text is what the command repeats.
    let (_, action) = short_failure(
        "python-foo",
        Some("pypi:foo"),
        &plain,
        0,
        &Ending::Remembered,
    );
    assert!(
        action
            .unwrap()
            .ends_with("auto-package build pypi:foo --force")
    );
    // A patch-driven ladder wrote no memo: nothing to force.
    let (_, action) = short_failure("python-foo", Some("foo"), &plain, 0, &Ending::Unremembered);
    assert_eq!(action, None);
    let (_, action) = short_failure(
        "python-foo",
        Some("foo"),
        &stepped,
        0,
        &Ending::Unremembered,
    );
    assert_eq!(action.as_deref(), Some("Do x."));
    // A branch build has no rerun command to name: its stored step only.
    let (_, action) = short_failure("python-foo", None, &stepped, 0, &Ending::Remembered);
    assert_eq!(action.as_deref(), Some("Do x."));
    let (_, action) = short_failure("python-foo", None, &plain, 0, &Ending::Remembered);
    assert_eq!(action, None);
    // A budget stop says to raise the budget, never --force.
    let budget = Ending::Budget {
        spent: 1.5,
        limit: 1.0,
    };
    let (reason, action) = short_failure("python-foo", Some("foo"), &plain, 0, &budget);
    assert!(
        reason.starts_with(
            "python-foo could not be resolved: stopped on the budget ($1.50 spent of $1.00)"
        ),
        "{reason}"
    );
    assert_eq!(action.as_deref(), Some("raise --budget and run again"));
    assert!(!reason.contains("--force"));
}

#[test]
fn a_step_keeps_a_dot_that_belongs_to_a_command_or_a_path() {
    assert_eq!(then_step(Some("Run make."), "x"), "Run make; x");
    assert_eq!(then_step(Some("Wait;"), "x"), "Wait; x");
    assert_eq!(then_step(Some("cd ."), "x"), "cd .; x");
    assert_eq!(then_step(Some("ls ./"), "x"), "ls ./; x");
    assert_eq!(then_step(None, "x"), "x");
}

#[test]
fn the_reason_is_capped_at_four_hundred_characters() {
    let long = body(&"word ".repeat(200), &[], None);
    let (reason, _) = short_failure("foo", Some("foo"), &long, 0, &Ending::Remembered);
    assert!((390..=400).contains(&reason.chars().count()), "{reason}");
    assert!(reason.ends_with('…'));
}

#[test]
fn a_long_headline_gives_way_before_the_refusal_does() {
    let refusal = "The wheel would drop the security patch fix-CVE-2024-1234.patch.";
    let recap = body(
        &format!("{}.", "long ".repeat(70).trim_end()),
        &[refusal, "3 resolution rounds did not fix it."],
        None,
    );
    let (reason, _) = short_failure("python-foo", Some("foo"), &recap, 1, &Ending::Remembered);
    assert!(reason.chars().count() <= 400, "{reason}");
    assert!(reason.ends_with(refusal), "{reason}");
    assert!(!reason.contains("resolution rounds"), "{reason}");
    assert!(reason.contains('…'), "{reason}");
}

#[test]
fn a_refused_wheel_puts_its_sentence_first_among_the_lines() {
    let lookups = std::cell::Cell::new(0);
    let case = refused_case(
        vec![
            blocked("reason one"),
            blocked("reason two"),
            blocked("reason three"),
        ],
        &lookups,
    );
    let (reason, kind, _) = failed(&case);
    assert_eq!(*kind, FailureKind::NeedsMaintainer);
    assert!(reason.chars().count() <= 400, "{reason}");
    let refusal = reason.find("fix-CVE-2024-1234.patch").expect(reason);
    let count = reason
        .find("3 resolution rounds did not fix it.")
        .expect(reason);
    assert!(refusal < count, "{reason}");
    let ws = Workspace::open(&case.state_root, "python-foo").unwrap();
    let summary = PkgState::load(&ws).ladder_recap.ladder_summary.unwrap();
    assert!(reason.ends_with(&summary), "{summary}");
}
