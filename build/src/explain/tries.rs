//! Tries: how many OBS builds a package needed in a run before it was built,
//! or before the run ended without building it.
//!
//! A try is one `Build` step started for the package. An outer retry (the
//! frontier running the package again after a failure) starts further builds
//! in the same run, so they count as further tries. A package that the run
//! finished with `AlreadyBuilt` and no build of its own is not counted.
//!
//! [`per_package`] is the one function behind `auto-package explain`, its
//! whole-run view and the end-of-run summary. The explain command reads the
//! events from `run.jsonl` ([`records_from_jsonl`]); the summary reads them
//! from the memory sink. Both feed the same records.

use std::collections::BTreeMap;

use ap_core::event::{BuildFailure, Event, PackageOutcome, StepKind, StepOutcome};
use ap_core::sink::pad_to;
use serde::Serialize;
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::humanise::humanise;
use super::render::wrap;

/// How many lines the summary's list of packages takes at most.
pub const SUMMARY_LINES: usize = 25;

/// Longest reason a line shows, in characters.
const REASON_MAX: usize = 140;

/// An event of a run, with the package it belongs to.
pub type PkgEvent = (String, Event);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriesOutcome {
    /// Built (on OBS or locally) in this run.
    Built,
    /// Not built: failed, waiting for dependencies, stopped for a maintainer,
    /// or skipped by OBS.
    NotBuilt,
    /// The run started builds of it but wrote no `package_finished` for it.
    NoResult,
    /// OBS already had it and this run built nothing for it.
    AlreadyBuilt,
}

/// One build of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Try {
    /// The build step's id within its package.
    pub step: u32,
    /// The round of the package's validate/build loop the build ran in.
    pub round: u32,
    /// `built`, `failed`, `skipped` or `not finished`.
    pub result: String,
    /// Why the try failed, in one line; `None` when it did not fail.
    pub reason: Option<String>,
}

/// The tries of one package in a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PkgTries {
    pub pkg: String,
    /// The builds the run started for the package.
    pub tries: usize,
    pub outcome: TriesOutcome,
    /// Why the package is not built; for a built one, why its last failed
    /// try failed.
    pub reason: Option<String>,
    /// The failure as the log classified it: the build failure kind, with the
    /// phase and genre of the log extract, or `DiskFull`.
    pub shape: Option<String>,
    pub attempts: Vec<Try>,
}

#[derive(Default)]
struct Acc {
    attempts: Vec<Try>,
    /// The last `BuildFailed` classification of the package.
    classification: Option<String>,
    /// The last log extract's `genre/phase`.
    extract: Option<String>,
    finished: Option<PackageOutcome>,
}

impl Acc {
    fn try_of(&mut self, step: u32) -> Option<&mut Try> {
        self.attempts.iter_mut().rev().find(|t| t.step == step)
    }

    fn shape(&self) -> Option<String> {
        match (&self.classification, &self.extract) {
            (Some(c), Some(x)) => Some(format!("{c} ({x})")),
            (Some(c), None) => Some(c.clone()),
            (None, Some(x)) => Some(format!("log {x}")),
            (None, None) => None,
        }
    }
}

/// The tries of every package the events name, in package order. A package
/// with neither a build nor a `package_finished` is not listed.
pub fn per_package(records: &[PkgEvent]) -> Vec<PkgTries> {
    let mut by_pkg: BTreeMap<String, Acc> = BTreeMap::new();
    for (ctx, event) in records {
        // A `package_finished` names its own package; the others take the
        // package the run tagged them with.
        let pkg = match event {
            Event::PackageFinished { pkg, .. } => pkg.as_str(),
            _ => ctx.as_str(),
        };
        if pkg.is_empty() {
            continue;
        }
        let acc = by_pkg.entry(pkg.to_string()).or_default();
        match event {
            Event::StepStarted {
                step,
                kind: StepKind::Build,
                round,
                ..
            } => acc.attempts.push(Try {
                step: step.0,
                round: *round,
                result: "not finished".into(),
                reason: None,
            }),
            Event::StepFinished { step, outcome, .. } => {
                if let Some(t) = acc.try_of(step.0) {
                    t.result = match outcome {
                        StepOutcome::Ok => "built",
                        StepOutcome::Failed => "failed",
                        StepOutcome::Skipped => "skipped",
                    }
                    .into();
                }
            }
            Event::BuildFailed {
                step,
                classification,
                ..
            } => {
                let label = failure_label(classification);
                if let Some(t) = acc.try_of(step.0) {
                    t.reason = Some(label.clone());
                }
                acc.classification = Some(label);
            }
            Event::ExtractResult { extract, .. } => {
                acc.extract = Some(format!(
                    "{}/{}",
                    extract.genre.as_deref().unwrap_or("?"),
                    extract.phase.as_deref().unwrap_or("?")
                ));
            }
            Event::PackageFinished { outcome, .. } => acc.finished = Some(outcome.clone()),
            _ => {}
        }
    }
    by_pkg
        .into_iter()
        .filter_map(|(pkg, acc)| finish(pkg, acc))
        .collect()
}

fn finish(pkg: String, mut acc: Acc) -> Option<PkgTries> {
    for t in &mut acc.attempts {
        if t.result == "failed" && t.reason.is_none() {
            t.reason = Some("failed, no classification recorded".into());
        }
    }
    let tries = acc.attempts.len();
    let last_failed = acc.attempts.iter().rev().find_map(|t| t.reason.clone());
    let (outcome, reason, shape) = match &acc.finished {
        Some(PackageOutcome::Built | PackageOutcome::BuiltLocally) => {
            (TriesOutcome::Built, last_failed, acc.shape())
        }
        Some(PackageOutcome::AlreadyBuilt) if tries > 0 => {
            (TriesOutcome::Built, last_failed, acc.shape())
        }
        Some(PackageOutcome::AlreadyBuilt) => (TriesOutcome::AlreadyBuilt, None, None),
        Some(PackageOutcome::Failed {
            reason, disk_full, ..
        }) => {
            if *disk_full {
                let why = format!("disk full: {reason}");
                (TriesOutcome::NotBuilt, Some(why), Some("DiskFull".into()))
            } else {
                (TriesOutcome::NotBuilt, Some(reason.clone()), acc.shape())
            }
        }
        Some(PackageOutcome::MissingDeps { deps }) => (
            TriesOutcome::NotBuilt,
            Some(format!("waiting for: {}", deps.join(", "))),
            acc.shape(),
        ),
        Some(
            PackageOutcome::NeedsMaintainer { reason, .. } | PackageOutcome::Skipped { reason },
        ) => (TriesOutcome::NotBuilt, Some(reason.clone()), acc.shape()),
        None if tries > 0 => (
            TriesOutcome::NoResult,
            Some("no package_finished: the run was interrupted or is still going".into()),
            acc.shape(),
        ),
        None => return None,
    };
    Some(PkgTries {
        pkg,
        tries,
        outcome,
        reason: reason.map(|r| short(&r)),
        shape,
        attempts: acc.attempts,
    })
}

/// The build failure as its one-line label.
fn failure_label(c: &BuildFailure) -> String {
    match c {
        BuildFailure::RpmParseError { line, message } => match line {
            Some(l) => format!("RpmParseError at line {l}: {}", short(message)),
            None => format!("RpmParseError: {}", short(message)),
        },
        BuildFailure::PrepFailed => "PrepFailed".into(),
        BuildFailure::BuildFailed => "BuildFailed".into(),
        BuildFailure::InstallFailed => "InstallFailed".into(),
        BuildFailure::CheckFailed => "CheckFailed".into(),
        BuildFailure::FilesNotOwned { .. } => "FilesNotOwned".into(),
        BuildFailure::UnresolvableDeps { packages } => {
            format!("UnresolvableDeps: {}", packages.join(", "))
        }
        BuildFailure::Unclassified { raw } => format!("Unclassified: {}", short(raw)),
    }
}

/// `text` on one line, at most [`REASON_MAX`] characters.
fn short(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    super::logs::truncate(&one, REASON_MAX)
}

/// The events of a `run.jsonl` the tries are read from, each with its package,
/// and how many of the lines naming such an event did not read as one.
pub fn records_from_jsonl(text: &str) -> (Vec<PkgEvent>, usize) {
    const KINDS: [&str; 5] = [
        "step_started",
        "step_finished",
        "build_failed",
        "extract_result",
        "package_finished",
    ];
    let mut records = Vec::new();
    let mut unreadable = 0;
    for line in text.lines() {
        if !KINDS
            .iter()
            .any(|k| line.contains(&format!("\"event\":\"{k}\"")))
        {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            unreadable += 1;
            continue;
        };
        let Ok(event) = serde_json::from_value::<Event>(v.clone()) else {
            unreadable += 1;
            continue;
        };
        let pkg = v["pkg"].as_str().unwrap_or_default().to_string();
        records.push((pkg, event));
    }
    (records, unreadable)
}

/// Whether any package of the run started a build.
pub fn ran_a_build(pkgs: &[PkgTries]) -> bool {
    pkgs.iter().any(|p| p.tries > 0)
}

/// The counts of the summary's distribution line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Distribution {
    pub one: usize,
    pub two: usize,
    pub three_plus: usize,
    pub not_built: usize,
}

/// How many packages were built with one, two or three or more tries, and how
/// many were not built. A package built without any build step, or already
/// built with none, is in none of them.
pub fn distribution(pkgs: &[PkgTries]) -> Distribution {
    let mut d = Distribution::default();
    for p in pkgs {
        match (p.outcome, p.tries) {
            (TriesOutcome::Built, 1) => d.one += 1,
            (TriesOutcome::Built, 2) => d.two += 1,
            (TriesOutcome::Built, n) if n >= 3 => d.three_plus += 1,
            (TriesOutcome::NotBuilt | TriesOutcome::NoResult, _) => d.not_built += 1,
            _ => {}
        }
    }
    d
}

/// The packages the summary lists: not built, or built with three or more
/// tries. Sorted by tries, most first, then by name.
pub fn flagged(pkgs: &[PkgTries]) -> Vec<&PkgTries> {
    let mut out: Vec<&PkgTries> = pkgs
        .iter()
        .filter(|p| match p.outcome {
            TriesOutcome::Built => p.tries >= 3,
            TriesOutcome::NotBuilt | TriesOutcome::NoResult => true,
            TriesOutcome::AlreadyBuilt => false,
        })
        .collect();
    out.sort_by(|a, b| b.tries.cmp(&a.tries).then_with(|| a.pkg.cmp(&b.pkg)));
    out
}

/// One line about a package: its tries and its last reason.
pub fn describe(p: &PkgTries) -> String {
    let n = match p.tries {
        1 => "1 try".to_string(),
        n => format!("{n} tries"),
    };
    match p.outcome {
        TriesOutcome::Built => match &p.reason {
            Some(r) => format!("{}: {n}, built; last failed try: {r}", p.pkg),
            None => format!("{}: {n}, built", p.pkg),
        },
        TriesOutcome::NotBuilt | TriesOutcome::NoResult => {
            let what = match p.outcome {
                TriesOutcome::NoResult => "no result",
                _ => "not built",
            };
            let shape = p
                .shape
                .as_ref()
                .map(|s| format!(" [{s}]"))
                .unwrap_or_default();
            let why = p.reason.as_deref().unwrap_or("no reason recorded");
            format!("{}: {n}, {what}{shape}: {why}", p.pkg)
        }
        TriesOutcome::AlreadyBuilt => format!("{}: already built, no build in this run", p.pkg),
    }
}

/// The block the end-of-run summary prints: a heading with how many packages
/// were tried, the buckets of their tries that are not empty, then the
/// [`flagged`] packages, at most [`SUMMARY_LINES`] lines of them, wrapped to
/// `width`. Empty when no package ran a build.
pub fn summary_block(pkgs: &[PkgTries], width: usize) -> String {
    use std::fmt::Write as _;
    if !ran_a_build(pkgs) {
        return String::new();
    }
    let d = distribution(pkgs);
    let total = d.one + d.two + d.three_plus + d.not_built;
    if total == 0 {
        return String::new();
    }
    let mut out = format!("\nTries to build ({total}):\n");
    let buckets: Vec<String> = [
        ("1 try", d.one),
        ("2 tries", d.two),
        ("3+ tries", d.three_plus),
        ("not built", d.not_built),
    ]
    .iter()
    .filter(|(_, n)| *n > 0)
    .map(|(label, n)| format!("{label}: {n}"))
    .collect();
    let _ = writeln!(out, "  {}", buckets.join(", "));

    let flagged = flagged(pkgs);
    let shown = if flagged.len() > SUMMARY_LINES {
        SUMMARY_LINES - 1
    } else {
        flagged.len()
    };
    let listed = &flagged[..shown];
    let name_width = listed
        .iter()
        .map(|p| UnicodeWidthStr::width(p.pkg.as_str()))
        .max()
        .unwrap_or(0);
    let column = 4 + name_width + 2;
    for p in listed {
        let detail = row_detail(p);
        let lines = wrap(&detail, width.saturating_sub(column));
        let mut lines = lines.iter();
        let first = lines.next().map(String::as_str).unwrap_or_default();
        let _ = writeln!(out, "    {}  {first}", pad_to(&p.pkg, name_width));
        for line in lines {
            let _ = writeln!(out, "{}{line}", " ".repeat(column));
        }
    }
    if flagged.len() > shown {
        let _ = writeln!(out, "    ... and {} more", flagged.len() - shown);
    }
    out
}

/// The detail of a package's row in the summary: its tries, its outcome and
/// the reason, with the failure kinds worded by [`humanise`].
fn row_detail(p: &PkgTries) -> String {
    let n = match p.tries {
        1 => "1 try".to_string(),
        n => format!("{n} tries"),
    };
    let shape = p
        .shape
        .as_ref()
        .map(|s| format!(" [{}]", humanise(s)))
        .unwrap_or_default();
    match p.outcome {
        TriesOutcome::Built => match &p.reason {
            Some(r) => format!("{n}, built · last failed try: {}", humanise(r)),
            None => format!("{n}, built"),
        },
        TriesOutcome::NotBuilt | TriesOutcome::NoResult => {
            let what = match p.outcome {
                TriesOutcome::NoResult => "no result",
                _ => "not built",
            };
            let why = p
                .reason
                .as_deref()
                .map_or_else(|| "no reason recorded".to_string(), humanise);
            format!("{n}, {what}{shape} · {why}")
        }
        TriesOutcome::AlreadyBuilt => format!("{n}, already built"),
    }
}
