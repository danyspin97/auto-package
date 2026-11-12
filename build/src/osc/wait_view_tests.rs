use super::wait_view::*;
use crate::osc::BuildStatus;
use std::time::Duration;

fn out(terminal: bool, quiet: bool, alone: bool) -> Output {
    Output {
        terminal,
        quiet,
        width: 100,
        colour: false,
        alone,
        first_poll: false,
    }
}

#[test]
fn the_first_poll_shows_no_live_line_and_the_second_one_does() {
    let mut logged = 0;
    let first = Output {
        first_poll: true,
        ..out(true, false, true)
    };
    assert_eq!(
        shown_while_waiting("building", Duration::ZERO, first, &mut logged),
        None
    );
    assert_eq!(
        shown_while_waiting(
            "building",
            Duration::from_secs(15),
            out(true, false, true),
            &mut logged
        ),
        Some(Shown::Live("  ▸ OBS build: building… 15.0s".to_string()))
    );
}

#[test]
fn a_building_package_is_warned_about_only_after_the_long_stall() {
    assert_eq!(
        warn_after(&BuildStatus::Building),
        Duration::from_secs(90 * 60)
    );
    // The 129s silence that used to produce the warning is no longer one.
    assert!(!warning_due(
        &BuildStatus::Building,
        Duration::from_secs(129)
    ));
    assert!(!warning_due(
        &BuildStatus::Building,
        Duration::from_secs(90 * 60 - 1)
    ));
    assert!(warning_due(
        &BuildStatus::Building,
        Duration::from_secs(90 * 60)
    ));
}

#[test]
fn other_states_that_do_not_change_keep_the_short_warning() {
    for status in [BuildStatus::Scheduled, BuildStatus::Blocked] {
        assert_eq!(warn_after(&status), NO_PROGRESS_WARNING, "{status:?}");
        assert!(warning_due(&status, Duration::from_secs(120)), "{status:?}");
        assert!(
            !warning_due(&status, Duration::from_secs(119)),
            "{status:?}"
        );
    }
}

#[test]
fn on_a_terminal_a_lone_wait_gets_the_live_line_and_no_log_line() {
    let mut logged = 0;
    let shown = shown_while_waiting(
        "building",
        Duration::from_secs(129),
        out(true, false, true),
        &mut logged,
    );
    assert_eq!(
        shown,
        Some(Shown::Live("  ▸ OBS build: building… 2m09s".to_string()))
    );
    assert_eq!(logged, 0);
}

#[test]
fn on_a_terminal_a_wait_beside_another_gets_a_line_a_minute() {
    let mut logged = 0;
    let mut at = |secs| {
        shown_while_waiting(
            "building",
            Duration::from_secs(secs),
            out(true, false, false),
            &mut logged,
        )
    };
    assert_eq!(at(59), None);
    assert_eq!(
        at(129),
        Some(Shown::Log("OBS build: building, 2m00s".to_string()))
    );
    assert_eq!(at(150), None);
    assert_eq!(
        at(180),
        Some(Shown::Log("OBS build: building, 3m00s".to_string()))
    );
}

#[test]
fn off_a_terminal_there_is_one_log_line_a_minute_and_no_live_line() {
    let mut logged = 0;
    let mut lines = Vec::new();
    // Polls every 15 seconds, as the wait makes them while the build holds still.
    for secs in (0..=240).step_by(15) {
        if let Some(shown) = shown_while_waiting(
            "building",
            Duration::from_secs(secs),
            out(false, false, true),
            &mut logged,
        ) {
            lines.push(shown);
        }
    }
    assert_eq!(
        lines,
        vec![
            Shown::Log("OBS build: building, 1m00s".to_string()),
            Shown::Log("OBS build: building, 2m00s".to_string()),
            Shown::Log("OBS build: building, 3m00s".to_string()),
            Shown::Log("OBS build: building, 4m00s".to_string()),
        ]
    );
}

#[test]
fn a_blocked_build_says_what_it_waits_for_in_its_log_line() {
    let mut logged = 0;
    let shown = shown_while_waiting(
        "blocked: libfoo",
        Duration::from_secs(61),
        out(false, false, true),
        &mut logged,
    );
    assert_eq!(
        shown,
        Some(Shown::Log("OBS build: blocked: libfoo, 1m00s".to_string()))
    );
}

#[test]
fn in_quiet_mode_nothing_is_shown_and_the_minute_is_not_used_up() {
    let mut logged = 0;
    assert_eq!(
        shown_while_waiting(
            "building",
            Duration::from_secs(600),
            out(true, true, true),
            &mut logged,
        ),
        None
    );
    assert_eq!(
        shown_while_waiting(
            "building",
            Duration::from_secs(600),
            out(false, true, true),
            &mut logged,
        ),
        None
    );
    assert_eq!(logged, 0);
}
