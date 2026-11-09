//! A package row's status, and the one function that moves it on: the
//! status cell, its glyph and colour all read from here.

use super::*;
use ap_core::sweep_event::SweepVerdict;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum PkgStatus {
    /// Found by the sweep, its check not answered yet: the state a row the
    /// sweep reports is created in.
    Checking,
    /// Checked: upstream has nothing newer.
    UpToDate,
    /// Checked: upstream has a newer version, and nothing queued the package
    /// (a dry run, or a source the pipeline cannot build from).
    NeedsUpdate,
    /// Checked, and not compared for a reason the Step column gives.
    CheckSkipped,
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
    /// Live, after a step that failed: the pipeline goes on to its next try,
    /// so the failure is not the package's end (that is `PackageFinished`).
    Retrying,
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
    /// The status as a word, the same for every row in it. The status cell
    /// adds the tries to a running one (see `PkgState::status_cell`).
    pub(super) fn label(&self) -> &'static str {
        match self {
            PkgStatus::Checking => "checking",
            PkgStatus::UpToDate => "up-to-date",
            PkgStatus::NeedsUpdate => "needs-update",
            PkgStatus::CheckSkipped => "skipped",
            PkgStatus::Queued => "queued",
            PkgStatus::Running | PkgStatus::Between => "running",
            PkgStatus::Retrying => "running (retrying)",
            PkgStatus::Succeeded => "done",
            PkgStatus::BuiltLocally => "done (local)",
            PkgStatus::AlreadyBuilt => "done (cached)",
            PkgStatus::MissingDeps => "waiting-upstream",
            PkgStatus::Failed => "failed",
            PkgStatus::NoSpace => "failed (no space)",
            PkgStatus::Skipped => "skipped",
        }
    }

    /// A one-column symbol in front of the label, so a row's state reads at a
    /// glance and still shows without colour. `tick` animates the running
    /// symbol.
    pub(super) fn glyph(&self, tick: u64) -> &'static str {
        match self {
            PkgStatus::Checking => "…",
            PkgStatus::UpToDate => "=",
            PkgStatus::NeedsUpdate => "↑",
            PkgStatus::CheckSkipped => "⊘",
            PkgStatus::Queued => "○",
            PkgStatus::Running => ["◐", "◓", "◑", "◒"][(tick % 4) as usize],
            PkgStatus::Between => "◌",
            PkgStatus::Retrying => "↻",
            PkgStatus::Succeeded | PkgStatus::BuiltLocally => "✓",
            PkgStatus::AlreadyBuilt => "✔",
            PkgStatus::MissingDeps => "◇",
            PkgStatus::Skipped => "⊘",
            PkgStatus::Failed | PkgStatus::NoSpace => "✗",
        }
    }

    pub(super) fn color(&self) -> Color {
        match self {
            PkgStatus::Checking => Color::Yellow,
            PkgStatus::UpToDate | PkgStatus::CheckSkipped => Color::DarkGray,
            PkgStatus::NeedsUpdate => Color::Cyan,
            PkgStatus::Queued => Color::DarkGray,
            PkgStatus::Running | PkgStatus::Between | PkgStatus::Retrying => Color::Yellow,
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
    /// `StepFinished` of the step the package is on; `failed` when it failed.
    StepFinished { notes: bool, failed: bool },
    /// `SweepChecking` for the package: its check has begun.
    Checking,
    /// `SweepProgress` for the package: what its check found.
    Checked(&'a SweepVerdict),
    /// `PackageFinished`. `disk_full` when an earlier step of this attempt
    /// already saw the disk run out (see `PkgState::disk_full_detected`).
    Finished {
        outcome: &'a PackageOutcome,
        disk_full: bool,
    },
}

/// The group a status is counted in by the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stage {
    /// What the sweep's check of the package found; never built.
    Check,
    Queued,
    Running,
    Done,
    Failed,
    /// Parked until the dependencies it is missing are built.
    Waiting,
    Skipped,
}

impl PkgStatus {
    /// The header group this status counts in.
    pub(super) fn stage(&self) -> Stage {
        match self {
            PkgStatus::Checking | PkgStatus::UpToDate | PkgStatus::NeedsUpdate => Stage::Check,
            PkgStatus::Queued => Stage::Queued,
            PkgStatus::Running | PkgStatus::Between | PkgStatus::Retrying => Stage::Running,
            PkgStatus::Succeeded | PkgStatus::BuiltLocally | PkgStatus::AlreadyBuilt => Stage::Done,
            PkgStatus::Failed | PkgStatus::NoSpace => Stage::Failed,
            PkgStatus::MissingDeps => Stage::Waiting,
            PkgStatus::Skipped | PkgStatus::CheckSkipped => Stage::Skipped,
        }
    }

    /// Where the row sorts in the table, lowest first: what is being worked
    /// on, then what is queued or waits for it, then what ended, the
    /// failures before the successes, and last what was only checked.
    pub(super) fn sort_rank(&self) -> u8 {
        match self {
            PkgStatus::Running | PkgStatus::Between | PkgStatus::Retrying | PkgStatus::Checking => {
                0
            }
            PkgStatus::Queued => 1,
            PkgStatus::MissingDeps => 2,
            PkgStatus::NeedsUpdate => 3,
            PkgStatus::Failed | PkgStatus::NoSpace => 4,
            PkgStatus::Succeeded | PkgStatus::BuiltLocally | PkgStatus::AlreadyBuilt => 5,
            PkgStatus::Skipped | PkgStatus::CheckSkipped => 6,
            PkgStatus::UpToDate => 7,
        }
    }

    /// Whether the row is only what the sweep's check found: nothing was
    /// queued for it, so the run does not wait on it to end.
    pub(super) fn is_check_only(&self) -> bool {
        matches!(
            self,
            PkgStatus::Checking
                | PkgStatus::UpToDate
                | PkgStatus::NeedsUpdate
                | PkgStatus::CheckSkipped
        )
    }

    /// Whether the package is built (here, locally, or already before).
    pub(super) fn is_built(&self) -> bool {
        self.stage() == Stage::Done
    }

    /// Whether the package's time runs: only while it is worked on.
    pub(super) fn timer_runs(&self) -> bool {
        self.stage() == Stage::Running || *self == PkgStatus::Checking
    }

    /// The status after `input`. This is the only place a row's status is
    /// decided; every handler that changes it goes through here.
    pub(super) fn next(&self, input: Input<'_>) -> PkgStatus {
        match input {
            // A check never takes a row back from the build it is in.
            Input::Checked(_) if !self.is_check_only() => self.clone(),
            Input::Checked(SweepVerdict::UpToDate { .. }) => PkgStatus::UpToDate,
            Input::Checked(SweepVerdict::Update { .. }) => PkgStatus::NeedsUpdate,
            Input::Checked(SweepVerdict::Skipped { .. }) => PkgStatus::CheckSkipped,
            // A check that begins takes a row only while it has no answer yet.
            Input::Checking => match self {
                PkgStatus::UpToDate | PkgStatus::NeedsUpdate | PkgStatus::CheckSkipped => {
                    self.clone()
                }
                _ if self.is_check_only() => PkgStatus::Checking,
                _ => self.clone(),
            },
            Input::Selected => PkgStatus::Queued,
            // The notes turn only runs after a successful build, so the
            // package is already done; showing it as running would hide that.
            Input::StepStarted { notes: true } | Input::StepFinished { notes: true, .. } => {
                PkgStatus::Succeeded
            }
            Input::StepStarted { notes: false } => PkgStatus::Running,
            // Only `PackageFinished` fails a package: a failed step is one
            // try, and the pipeline decides whether another follows.
            Input::StepFinished {
                notes: false,
                failed: true,
            } => PkgStatus::Retrying,
            Input::StepFinished {
                notes: false,
                failed: false,
            } => PkgStatus::Between,
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

/// How many rows are in each [`Stage`], for the header.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct StageCounts {
    pub(super) queued: usize,
    pub(super) running: usize,
    pub(super) done: usize,
    pub(super) failed: usize,
    pub(super) waiting: usize,
    pub(super) skipped: usize,
    /// The checks begun and not answered yet: the sweep's rows in `checking`.
    pub(super) checking: usize,
}

impl StageCounts {
    pub(super) fn of<'a>(statuses: impl IntoIterator<Item = &'a PkgStatus>) -> Self {
        let mut counts = StageCounts::default();
        for status in statuses {
            if *status == PkgStatus::Checking {
                counts.checking += 1;
            }
            let slot = match status.stage() {
                Stage::Check => continue,
                Stage::Queued => &mut counts.queued,
                Stage::Running => &mut counts.running,
                Stage::Done => &mut counts.done,
                Stage::Failed => &mut counts.failed,
                Stage::Waiting => &mut counts.waiting,
                Stage::Skipped => &mut counts.skipped,
            };
            *slot += 1;
        }
        counts
    }
}
