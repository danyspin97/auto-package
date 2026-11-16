use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::event::{StepId, StepKind, StepOutcome};

fn started(step: u32, kind: StepKind) -> Event {
    Event::StepStarted {
        step: StepId(step),
        kind,
        attempt: 1,
        round: 0,
        detail: None,
    }
}

#[test]
fn the_memory_sink_keeps_the_build_steps_of_each_package_and_nothing_else() {
    let sink = Arc::new(MemorySink::new());
    let foo = PkgContextReporter::new(sink.clone(), Some("foo".into()));
    let bar = PkgContextReporter::new(sink.clone(), Some("bar".into()));
    foo.report(&started(0, StepKind::Checkout));
    foo.report(&started(1, StepKind::Build));
    bar.report(&started(0, StepKind::Build));
    // The same id in another package is another step.
    foo.report(&Event::StepFinished {
        step: StepId(0),
        outcome: StepOutcome::Ok,
        elapsed: Duration::ZERO,
    });
    bar.report(&Event::StepFinished {
        step: StepId(0),
        outcome: StepOutcome::Failed,
        elapsed: Duration::ZERO,
    });
    foo.report(&Event::PackageFinished {
        pkg: "foo".into(),
        version: "1.0".into(),
        outcome: PackageOutcome::Built,
    });
    let kept: Vec<(String, String)> = sink
        .tries_records()
        .into_iter()
        .map(|(pkg, e)| {
            let name = match e {
                Event::StepStarted { .. } => "started",
                Event::StepFinished { .. } => "finished",
                Event::PackageFinished { .. } => "package_finished",
                _ => "other",
            };
            (pkg, name.to_string())
        })
        .collect();
    assert_eq!(
        kept,
        vec![
            ("foo".to_string(), "started".to_string()),
            ("bar".to_string(), "started".to_string()),
            ("bar".to_string(), "finished".to_string()),
            ("foo".to_string(), "package_finished".to_string()),
        ]
    );
    // foo's Checkout step (step 0) is not a build, so neither its start nor
    // its end is kept.
    assert_eq!(sink.finished().len(), 1);
}

#[test]
fn the_memory_sink_keeps_the_packages_selected_in_order() {
    let sink = MemorySink::new();
    for pkg in ["foo", "bar"] {
        sink.report(&Event::TargetSelected {
            pkg: pkg.into(),
            version: "1.0".into(),
            reason: crate::event::SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        });
    }
    assert_eq!(sink.selected(), ["foo", "bar"]);
    assert!(sink.finished().is_empty());
}
