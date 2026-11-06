//! A package row's status, and the one function that moves it on: the
//! status cell, its glyph and colour all read from here.

use super::*;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum PkgStatus {
    Queued,
    Running,
    Succeeded,
    /// Built on this machine by `--local`: OBS has no build of it, so it is
    /// not offered for a submit request.
    BuiltLocally,
    AlreadyBuilt,
    MissingDeps,
    Failed,
    /// Live, but no step is executing right now: the last step has finished
    /// and the agent turn (or `PackageFinished`) has not yet come in.
    Between,
    /// `Failed` whose build log signature matched [`looks_like_disk_full`] —
    /// split out from plain `Failed` so it's visually distinct in the table
    /// (nothing about the spec is wrong; there's nothing to fix but disk
    /// space) and so the `r` keybinding knows which row it applies to.
    NoSpace,
    /// Excluded by OBS for this repository and arch: nothing was built, and
    /// the reason is in the detail pane.
    Skipped,
}

impl PkgStatus {
    pub(super) fn label(&self) -> &'static str {
        match self {
            PkgStatus::Queued => "queued",
            PkgStatus::Running => "running",
            PkgStatus::Between => "working",
            PkgStatus::Succeeded => "done",
            PkgStatus::BuiltLocally => "built locally",
            PkgStatus::AlreadyBuilt => "cached",
            PkgStatus::MissingDeps => "blocked",
            PkgStatus::Failed => "failed",
            PkgStatus::NoSpace => "no space",
            PkgStatus::Skipped => "skipped",
        }
    }

    /// A one-column symbol in front of the label, so a row's state reads at a
    /// glance and still shows without colour. `tick` animates the running
    /// symbol.
    pub(super) fn glyph(&self, tick: u64) -> &'static str {
        match self {
            PkgStatus::Queued => "○",
            PkgStatus::Running => ["◐", "◓", "◑", "◒"][(tick % 4) as usize],
            PkgStatus::Between => "◌",
            PkgStatus::Succeeded | PkgStatus::BuiltLocally => "✓",
            PkgStatus::AlreadyBuilt => "✔",
            PkgStatus::MissingDeps => "◇",
            PkgStatus::Skipped => "⊘",
            PkgStatus::Failed | PkgStatus::NoSpace => "✗",
        }
    }

    pub(super) fn color(&self) -> Color {
        match self {
            PkgStatus::Queued => Color::DarkGray,
            PkgStatus::Running | PkgStatus::Between => Color::Yellow,
            PkgStatus::Succeeded | PkgStatus::BuiltLocally | PkgStatus::AlreadyBuilt => {
                Color::Green
            }
            PkgStatus::MissingDeps => Color::Magenta,
            PkgStatus::Failed => Color::Red,
            PkgStatus::NoSpace => Color::LightRed,
            PkgStatus::Skipped => Color::DarkGray,
        }
    }
}

/// What happened to a package, as far as its status is concerned.
#[derive(Clone, Copy)]
pub(super) enum Input<'a> {
    /// `TargetSelected`: the package waits for its build, again for a retry.
    Selected,
    /// `StepStarted`. `notes` for the notes turn, which only runs once the
    /// package has built.
    StepStarted { notes: bool },
    /// `StepFinished` of the step the package is on.
    StepFinished { notes: bool },
    /// `PackageFinished`. `disk_full` when an earlier step of this attempt
    /// already saw the disk run out (see `PkgState::disk_full_detected`).
    Finished {
        outcome: &'a PackageOutcome,
        disk_full: bool,
    },
}

impl PkgStatus {
    /// Whether the package's time runs: only while it is worked on.
    pub(super) fn timer_runs(&self) -> bool {
        matches!(self, PkgStatus::Running | PkgStatus::Between)
    }

    /// The status after `input`. This is the only place a row's status is
    /// decided; every handler that changes it goes through here.
    pub(super) fn next(&self, input: Input<'_>) -> PkgStatus {
        match input {
            Input::Selected => PkgStatus::Queued,
            // The notes turn only runs after a successful build, so the
            // package is already done; showing it as running would hide that.
            Input::StepStarted { notes: true } | Input::StepFinished { notes: true } => {
                PkgStatus::Succeeded
            }
            Input::StepStarted { notes: false } => PkgStatus::Running,
            Input::StepFinished { notes: false } => PkgStatus::Between,
            Input::Finished { outcome, disk_full } => match outcome {
                PackageOutcome::Built => PkgStatus::Succeeded,
                PackageOutcome::BuiltLocally => PkgStatus::BuiltLocally,
                PackageOutcome::AlreadyBuilt => PkgStatus::AlreadyBuilt,
                PackageOutcome::MissingDeps { .. } => PkgStatus::MissingDeps,
                PackageOutcome::Failed { disk_full: d, .. } if *d || disk_full => {
                    PkgStatus::NoSpace
                }
                PackageOutcome::Failed { .. } | PackageOutcome::NeedsMaintainer { .. } => {
                    PkgStatus::Failed
                }
                PackageOutcome::Skipped { .. } => PkgStatus::Skipped,
            },
        }
    }
}

/// A package's time, banked whenever its status stops the clock.
#[derive(Clone, Copy, Default)]
pub(super) struct Clock {
    /// When the clock last started, while it runs.
    pub(super) running_since: Option<Instant>,
    /// The time counted before `running_since`; `None` until it first ran.
    pub(super) banked: Option<Duration>,
}

impl Clock {
    /// Starts the clock when `runs` and it is stopped; stops it, keeping the
    /// time, when not.
    pub(super) fn follow(&mut self, runs: bool) {
        match (runs, self.running_since) {
            (true, None) => self.running_since = Some(Instant::now()),
            (false, Some(since)) => {
                self.banked = Some(self.banked.unwrap_or_default() + since.elapsed());
                self.running_since = None;
            }
            _ => {}
        }
    }

    /// The time so far, or `None` if the clock never ran.
    pub(super) fn elapsed(&self) -> Option<Duration> {
        match (self.running_since, self.banked) {
            (None, None) => None,
            (since, banked) => {
                Some(banked.unwrap_or_default() + since.map_or(Duration::ZERO, |s| s.elapsed()))
            }
        }
    }
}
