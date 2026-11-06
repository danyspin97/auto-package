//! The package row's badge for a built package that fails on another arch.

use super::*;
use ap_core::event::{ArchCheckResult, Event, SelectionReason};

fn result(repo: &str, arch: &str, state: &str) -> ArchCheckResult {
    ArchCheckResult {
        repo: repo.into(),
        arch: arch.into(),
        state: state.into(),
        detail: None,
        origin_fails: false,
        origin_missing: false,
        log: None,
    }
}

fn built_then_checked(results: Vec<ArchCheckResult>) -> TuiState {
    let mut s = TuiState::new(RunScope::default());
    let finished = Event::PackageFinished {
        pkg: "libdfp".into(),
        version: "1".into(),
        outcome: PackageOutcome::Built,
    };
    s.apply(Msg {
        pkg: None,
        event: finished,
    });
    s.apply(Msg {
        pkg: None,
        event: Event::ArchCheck {
            package: "libdfp".into(),
            results,
        },
    });
    s
}

#[test]
fn a_built_package_that_fails_on_another_arch_shows_a_short_badge_and_the_full_log_line() {
    let s = built_then_checked(vec![
        result("openSUSE_Factory", "riscv64", "failed"),
        result("openSUSE_Factory", "aarch64", "succeeded"),
    ]);
    let pkg = &s.packages["libdfp"];
    assert_eq!(pkg.status_cell(), "done ⚠");
    assert!(
        pkg.log_lines
            .iter()
            .any(|l| l.message == "built (fails on openSUSE_Factory/riscv64)"),
        "the full names are in the log"
    );
}

#[test]
fn a_package_with_no_failing_arch_keeps_its_plain_label() {
    let s = built_then_checked(vec![result("openSUSE_Factory", "aarch64", "succeeded")]);
    assert_eq!(s.packages["libdfp"].status_cell(), "done");
}

#[test]
fn the_status_cell_fits_the_status_column() {
    let width = panes::TABLE_COLUMNS[0].1.unwrap() as usize;
    // The glyph and its space come before the text in the cell.
    let text_room = width - 2;
    let mut built = PkgState::new("1".into());
    built.status = PkgStatus::Succeeded;
    built.arch_fails = vec!["openSUSE_Factory_RISCV/riscv64".into(); 3];
    let mut retrying = PkgState::new("1".into());
    retrying.status = PkgStatus::Retrying;
    (retrying.tries, retrying.max_tries) = (2, 3);
    for pkg in [built, retrying] {
        let cell = pkg.status_cell();
        assert!(
            cell.chars().count() <= text_room,
            "{cell:?} is {} characters, the column leaves {text_room}",
            cell.chars().count()
        );
    }
}

#[test]
fn a_new_attempt_forgets_the_arches_the_last_build_failed_on() {
    let mut s = built_then_checked(vec![result("openSUSE_Factory", "riscv64", "failed")]);
    assert_eq!(s.packages["libdfp"].arch_fails.len(), 1);
    s.apply(Msg {
        pkg: None,
        event: Event::TargetSelected {
            pkg: "libdfp".into(),
            version: "1".into(),
            reason: SelectionReason::RequestedRoot,
            blocked_by: Vec::new(),
            from_version: None,
        },
    });
    assert!(s.packages["libdfp"].arch_fails.is_empty());
    assert_eq!(s.packages["libdfp"].status_cell(), "queued");
}
