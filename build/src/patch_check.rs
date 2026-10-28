//! Test a spec's patches against the unpacked new sources with real quilt.
//!
//! A version bump keeps the recipe's patches (see `carry_forward_spec`); some no
//! longer apply to the new release. This module builds the quilt series from
//! the spec's `PatchN:` lines in the order rpm applies them, honouring each
//! patch's `-pN`, pushes it one patch at a time on a scratch copy of the source
//! archive and classifies every patch. The checkout and the workspace's
//! `source/` are never touched: everything happens in a temporary directory.
//!
//! A patch is never classified as removable because it fails: only quilt's
//! own `Patch X can be reverse-applied` (its change is already in the new
//! sources) is [`PatchStatus::AlreadyApplied`], the one proof that removing a
//! patch is right.
//!
//! The parsing of quilt's output, the series and the agent's report are pure
//! functions, tested without a quilt binary.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The strip levels the check runs (the ones rpm's macros use in practice).
const SUPPORTED_LEVELS: [u32; 2] = [0, 1];
/// What one patch's quilt output and its rejected hunks keep for the report.
const OUTPUT_LIMIT: usize = 2000;
const REJECTS_LIMIT: usize = 4000;
/// Rounds of agent turns the pipeline gives the failing patches.
pub const MAX_ROUNDS: usize = 4;
/// What quilt says when a patch that does not apply is already in the tree.
const REVERSE_APPLIED: &str = "can be reverse-applied";

/// One patch of the series, as the spec applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEntry {
    /// The `PatchN:` number (`"0"` for a bare `Patch:`).
    pub number: String,
    /// The file name, with `%{name}` and `%{version}` expanded.
    pub name: String,
    /// The `-pN` level the spec applies it with.
    pub level: u32,
}

/// A patch of the spec, either checked or with the reason it cannot be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Check(SeriesEntry),
    Skip { name: String, reason: String },
}

/// What quilt said about one patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchStatus {
    /// `quilt push` applied it with no offset and no fuzz.
    Applies,
    /// It applied, but with an offset or fuzz: `quilt refresh` makes it exact
    /// (a fuzzy patch fails rpm's `%autosetup`, which runs with `--fuzz=0`).
    AppliesWithOffsetOrFuzz { fuzz: bool, output: String },
    /// It does not apply: quilt's output and the rejected hunks. Never a
    /// reason to remove the patch: it is rebased.
    Fails { output: String, rejects: String },
    /// It does not apply because its change is already in the sources: quilt
    /// said `can be reverse-applied`, also with `--fuzz=0`. The proof that the
    /// patch may be removed.
    AlreadyApplied { output: String },
    /// It was not tried, and why. Never a success.
    NotChecked { reason: String },
}

/// One patch's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchResult {
    pub name: String,
    pub level: u32,
    pub status: PatchStatus,
    /// The patch as `quilt refresh` rewrote it, when the check was asked to
    /// refresh fuzzy patches (see [`Refresh`]).
    pub refreshed: Option<Vec<u8>>,
}

/// Counts that decide whether another round is worth it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub failing: usize,
    pub refresh: usize,
    /// Patches proven already applied upstream (to be removed).
    pub upstream: usize,
}

/// The verdict on every patch of the spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// Every `PatchN:` of the spec, in series order.
    pub patches: Vec<PatchResult>,
    /// Why quilt did not run at all (no quilt, no archive, an archive that
    /// would not unpack). The patches are then all `NotChecked`.
    pub not_run: Option<String>,
}

impl CheckReport {
    pub fn failing(&self) -> impl Iterator<Item = &PatchResult> {
        self.patches
            .iter()
            .filter(|p| matches!(p.status, PatchStatus::Fails { .. }))
    }

    pub fn to_refresh(&self) -> impl Iterator<Item = &PatchResult> {
        self.patches
            .iter()
            .filter(|p| matches!(p.status, PatchStatus::AppliesWithOffsetOrFuzz { .. }))
    }

    pub fn already_applied(&self) -> impl Iterator<Item = &PatchResult> {
        self.patches
            .iter()
            .filter(|p| matches!(p.status, PatchStatus::AlreadyApplied { .. }))
    }

    pub fn tally(&self) -> Tally {
        Tally {
            failing: self.failing().count(),
            refresh: self.to_refresh().count(),
            upstream: self.already_applied().count(),
        }
    }

    /// Whether quilt proved `name` already applied in the sources.
    pub fn proves_applied(&self, name: &str) -> bool {
        self.already_applied().any(|p| p.name == name)
    }

    /// Nothing fails and nothing needs a refresh. Unchecked patches are not
    /// problems quilt found, but they are not called clean either: see
    /// [`CheckReport::summary`].
    pub fn is_clean(&self) -> bool {
        self.tally() == Tally::default()
    }

    /// One line for the log.
    pub fn summary(&self) -> String {
        if let Some(why) = &self.not_run {
            return format!("the patches were not checked with quilt: {why}");
        }
        let count =
            |f: fn(&PatchStatus) -> bool| self.patches.iter().filter(|p| f(&p.status)).count();
        let clean = count(|s| matches!(s, PatchStatus::Applies));
        let refresh = count(|s| matches!(s, PatchStatus::AppliesWithOffsetOrFuzz { .. }));
        let failing = count(|s| matches!(s, PatchStatus::Fails { .. }));
        let upstream = count(|s| matches!(s, PatchStatus::AlreadyApplied { .. }));
        let skipped: Vec<String> = self
            .patches
            .iter()
            .filter_map(|p| match &p.status {
                PatchStatus::NotChecked { reason } => Some(format!("{} ({reason})", p.name)),
                _ => None,
            })
            .collect();
        let mut text = format!(
            "quilt: {clean} of {} patch(es) apply cleanly, {refresh} need a refresh, {failing} fail",
            self.patches.len()
        );
        if upstream > 0 {
            text.push_str(&format!(", {upstream} already applied upstream"));
        }
        if !skipped.is_empty() {
            text.push_str(&format!("; not checked: {}", skipped.join(", ")));
        }
        text
    }
}

/// Whether the check rewrites fuzzy patches with `quilt refresh` and returns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    No,
    /// Refresh each patch that applied with fuzz (the pipeline's last resort
    /// when no agent turn refreshed it; an offset alone is left as it is).
    Fuzzy,
}

/// The `PatchN:` lines of `spec`, as `(number, value)`; a bare `Patch:` is 0.
fn patch_lines(spec: &str) -> Vec<(String, String)> {
    spec.lines()
        .filter_map(|l| {
            let (tag, value) = l.split_once(':')?;
            let tag = tag.trim();
            let is_patch = tag.len() >= 5
                && tag
                    .get(..5)
                    .is_some_and(|p| p.eq_ignore_ascii_case("patch"))
                && tag[5..].chars().all(|c| c.is_ascii_digit());
            is_patch.then(|| {
                let number = if tag.len() == 5 { "0" } else { &tag[5..] };
                (number.to_string(), value.trim().to_string())
            })
        })
        .collect()
}

/// The `PatchN:` of `spec` as `(number, file name)`, the name expanded as the
/// check expands it.
pub fn declared_patches(spec: &str) -> Vec<(String, String)> {
    patch_lines(spec)
        .into_iter()
        .map(|(n, raw)| (n, crate::pipeline::expand_patch_name(&raw, spec)))
        .collect()
}

/// What a turn did to the patches: `(refreshed, removed)`. `before` holds each
/// patch the spec declared before the turn with its file's content (`None`
/// when not on disk); `after_names` are the patches the spec declares after it
/// and `after` their contents now. A patch is removed when the spec no longer
/// declares it or its file is gone, refreshed when its content changed.
pub fn diff_patches(
    before: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    after_names: &[String],
    after: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
) -> (Vec<String>, Vec<String>) {
    let mut refreshed = Vec::new();
    let mut removed = Vec::new();
    for (name, old) in before {
        let now = after.get(name).cloned().flatten();
        if !after_names.contains(name) || (old.is_some() && now.is_none()) {
            removed.push(name.clone());
        } else if old.is_some() && now != *old {
            refreshed.push(name.clone());
        }
    }
    (refreshed, removed)
}

/// The spec's patches in the order rpm applies them, each with what the check
/// does with it. `%autosetup` and `%autopatch` apply by patch number; a spec
/// with only `%patch` lines applies in the order of those lines.
pub fn plan_series(spec: &str) -> Vec<Planned> {
    let lines: Vec<&str> = spec.lines().collect();
    let first = |l: &str| l.split_whitespace().next().unwrap_or("").to_string();
    let auto = lines.iter().any(|l| {
        let f = first(l);
        f == "%autopatch" || (f == "%autosetup" && !l.split_whitespace().any(|t| t == "-N"))
    });
    let patch_line_index = |number: &str| {
        lines.iter().position(|l| {
            first(l).starts_with("%patch")
                && crate::pipeline::applied_patch_number(l)
                    .is_some_and(|n| crate::pipeline::same_patch_number(&n, number))
        })
    };
    let mut planned: Vec<(usize, u64, Planned)> = Vec::new();
    for (number, raw) in patch_lines(spec) {
        let name = crate::pipeline::expand_patch_name(&raw, spec);
        let numeric: u64 = number.parse().unwrap_or(u64::MAX);
        let explicit = patch_line_index(&number);
        let skip = |reason: String| Planned::Skip {
            name: name.clone(),
            reason,
        };
        let plan = if name.is_empty()
            || name.contains('%')
            || Path::new(&name).file_name() != Some(name.as_ref())
        {
            skip(format!(
                "`{raw}` is not a plain file name once %{{name}} and %{{version}} are expanded"
            ))
        } else if !auto && explicit.is_none() {
            skip("the spec does not apply it (no %autosetup, %autopatch or %patch line)".into())
        } else {
            let level = crate::pipeline::spec_patch_level(spec, &number);
            if SUPPORTED_LEVELS.contains(&level) {
                Planned::Check(SeriesEntry {
                    number: number.clone(),
                    name: name.clone(),
                    level,
                })
            } else {
                skip(format!(
                    "it applies with -p{level}; only -p0 and -p1 are checked"
                ))
            }
        };
        let order = if auto {
            0
        } else {
            explicit.unwrap_or(usize::MAX)
        };
        planned.push((order, numeric, plan));
    }
    // A stable sort: equal keys keep the spec's order.
    planned.sort_by_key(|(order, numeric, _)| if auto { (0, *numeric) } else { (*order, 0) });
    planned.into_iter().map(|(_, _, p)| p).collect()
}

/// The quilt `series` file for `entries`: one name per line, with its `-pN`
/// when that is not quilt's default `-p1`.
pub fn series_text(entries: &[SeriesEntry]) -> String {
    entries
        .iter()
        .map(|e| {
            if e.level == 1 {
                format!("{}\n", e.name)
            } else {
                format!("{} -p{}\n", e.name, e.level)
            }
        })
        .collect()
}

/// What one `quilt push` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushVerdict {
    Clean,
    OffsetOrFuzz {
        fuzz: bool,
    },
    Failed,
    /// It failed, and quilt says the change is already in the tree.
    ReverseApplied,
}

/// Read a `quilt push` of one patch: its exit status and output.
pub fn classify_push(success: bool, output: &str) -> PushVerdict {
    if !success {
        return if output.lines().any(|l| l.contains(REVERSE_APPLIED)) {
            PushVerdict::ReverseApplied
        } else {
            PushVerdict::Failed
        };
    }
    let fuzz = output.lines().any(|l| l.contains(" with fuzz "));
    let offset = output.lines().any(|l| l.contains("(offset "));
    if fuzz || offset {
        PushVerdict::OffsetOrFuzz { fuzz }
    } else {
        PushVerdict::Clean
    }
}

/// The reject files a `quilt push` (or `quilt push -f`) names, relative to the
/// tree: `... -- rejects in file foo.c` and `... -- saving rejects to file
/// foo.c.rej`.
pub fn reject_files(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let file = if let Some((_, rest)) = line.split_once("saving rejects to file ") {
            rest.trim().to_string()
        } else if let Some((_, rest)) = line.split_once("rejects in file ") {
            format!("{}.rej", rest.trim())
        } else {
            continue;
        };
        if !file.is_empty() && !out.contains(&file) {
            out.push(file);
        }
    }
    out
}

/// `text` cut to about `limit` bytes on a character boundary, saying so.
pub fn truncate(text: &str, limit: usize) -> String {
    let text = text.trim_end();
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[... {} more bytes cut]",
        &text[..end],
        text.len() - end
    )
}

/// The report the agent turn is given: every patch with quilt's verdict, the
/// output and rejected hunks of a failing one, the hints, and where things are.
/// `hints` are `(patch, directory)` for patches whose paths name another
/// release's tree (`stale_patches`).
pub fn agent_report(
    report: &CheckReport,
    version: &str,
    hints: &[(String, String)],
    checkout: &Path,
) -> String {
    let mut text = format!(
        "quilt was run on a pristine copy of the {version} sources, the patches in \
         series order:\n"
    );
    for p in &report.patches {
        let level = if p.level == 1 {
            String::new()
        } else {
            format!(" (-p{})", p.level)
        };
        match &p.status {
            PatchStatus::Applies => text.push_str(&format!(
                "\n- {}{level}: applies cleanly; leave it.\n",
                p.name
            )),
            PatchStatus::AppliesWithOffsetOrFuzz { fuzz, output } => text.push_str(&format!(
                "\n- {}{level}: applies with {}: refresh it.\n{}",
                p.name,
                if *fuzz {
                    "fuzz (rpm applies patches with --fuzz=0, so it FAILS the build)"
                } else {
                    "an offset"
                },
                crate::agent::fenced_data(output)
            )),
            PatchStatus::Fails { output, rejects } => {
                text.push_str(&format!(
                    "\n- {}{level}: FAILS to apply. quilt said:\n{}",
                    p.name,
                    crate::agent::fenced_data(output)
                ));
                if !rejects.is_empty() {
                    text.push_str(&format!(
                        "Rejected hunks:\n{}",
                        crate::agent::fenced_data(rejects)
                    ));
                }
                if let Some((_, dir)) = hints.iter().find(|(n, _)| *n == p.name) {
                    text.push_str(&format!(
                        "Its paths name `{dir}`, another release's tree: rebase it onto the \
                         {version} tree like the others.\n"
                    ));
                }
            }
            PatchStatus::AlreadyApplied { .. } => text.push_str(&format!(
                "\n- {}{level}: already applied upstream (quilt: `{REVERSE_APPLIED}`); the \
                 pipeline removes it.\n",
                p.name
            )),
            PatchStatus::NotChecked { reason } => text.push_str(&format!(
                "\n- {}{level}: not checked ({reason}); leave it.\n",
                p.name
            )),
        }
    }
    text.push_str(&format!(
        "\nThe new sources are `source/` in the workspace. The patches are files in the \
         checkout {} under their own names (QUILT_PATCHES points there, and its `series` \
         lists them in the order above), so `quilt refresh` in `source/` writes the patch \
         back in place.\n\
         A patch is never removed because it does not apply, nor because it only touches \
         tests (a failing hunk in a `.result` or test file is rebased like any other). The \
         only accepted proof is quilt's own: `quilt push` of the unchanged patch on the \
         pristine sources prints `Patch <name> {REVERSE_APPLIED}`. A removal without it is \
         rejected and rolled back.\n",
        checkout.display()
    ));
    text
}

/// What the loop does after a round of agent turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextRound {
    /// Every patch applies cleanly.
    Clean,
    /// Something still fails and the round made progress: ask again.
    Again,
    /// The round failed nothing fewer and changed no patch.
    NoProgress,
    /// The rounds are spent.
    OutOfRounds,
}

/// The loop's stopping rule. `rounds_done` counts the rounds run so far
/// (this one included); `before` and `after` are the quilt tallies around the
/// round; `patches_changed` says whether the round changed or removed a patch.
pub fn next_round(
    rounds_done: usize,
    max_rounds: usize,
    before: Tally,
    after: Tally,
    patches_changed: bool,
) -> NextRound {
    if after == Tally::default() {
        return NextRound::Clean;
    }
    if !(after.failing < before.failing || patches_changed) {
        return NextRound::NoProgress;
    }
    if rounds_done >= max_rounds {
        return NextRound::OutOfRounds;
    }
    NextRound::Again
}

/// The `.changes` bullet for a refreshed patch. The packaged version is not
/// named: the release's `Update to` entry already does.
pub fn refresh_bullet(name: &str) -> String {
    format!("Refresh {name}")
}

/// The `.changes` bullet for a patch removed because quilt proved it already
/// applied upstream: the bullet states the proof.
pub fn proven_drop_bullet(name: &str) -> String {
    format!("Drop {name}: already applied upstream (reverse-applies cleanly)")
}

/// Where the quilt binary is, or `None`.
fn quilt_available() -> bool {
    std::process::Command::new("quilt")
        .arg("--version")
        .env("QUILTRC", "/dev/null")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A report with every planned patch not checked, for `why`.
fn not_run(plan: &[Planned], why: String) -> CheckReport {
    CheckReport {
        patches: plan
            .iter()
            .map(|p| match p {
                Planned::Check(e) => PatchResult {
                    name: e.name.clone(),
                    level: e.level,
                    status: PatchStatus::NotChecked {
                        reason: why.clone(),
                    },
                    refreshed: None,
                },
                Planned::Skip { name, reason } => PatchResult {
                    name: name.clone(),
                    level: 1,
                    status: PatchStatus::NotChecked {
                        reason: reason.clone(),
                    },
                    refreshed: None,
                },
            })
            .collect(),
        not_run: Some(why),
    }
}

/// One quilt invocation in the scratch tree.
struct Quilt {
    root: PathBuf,
    patches: PathBuf,
    env: std::collections::BTreeMap<String, String>,
    scratch_text: String,
}

impl Quilt {
    fn run(&self, args: &[&str]) -> Result<(bool, String), String> {
        let out = crate::proc::capture_with_timeout_env(
            "quilt",
            args,
            &self.root,
            crate::proc::Emit::quiet(),
            Duration::from_secs(120),
            Some(&self.env),
        )
        .map_err(|e| format!("could not run quilt {}: {e}", args.join(" ")))?;
        let text = format!("{}{}", out.stdout, out.stderr).replace(&self.scratch_text, "");
        Ok((out.success(), text))
    }

    fn top(&self) -> Option<String> {
        self.run(&["top"])
            .ok()
            .filter(|(ok, _)| *ok)
            .map(|(_, t)| t.trim().rsplit('/').next().unwrap_or("").to_string())
    }
}

/// Run quilt over the spec's patches on a scratch copy of `tarball`. The patch
/// files are read from `checkout` and copied: the checkout is never written.
pub fn check(spec: &str, checkout: &Path, tarball: Option<&Path>, refresh: Refresh) -> CheckReport {
    check_with(
        spec,
        &|name| std::fs::read(checkout.join(name)).ok(),
        tarball,
        refresh,
    )
}

/// [`check`] with the patch files given by name (`None`: not there), for a
/// state of the patches that is no longer on disk (the one before a turn).
pub fn check_files(
    spec: &str,
    files: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    tarball: Option<&Path>,
) -> CheckReport {
    check_with(
        spec,
        &|name| files.get(name).cloned().flatten(),
        tarball,
        Refresh::No,
    )
}

fn check_with(
    spec: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    tarball: Option<&Path>,
    refresh: Refresh,
) -> CheckReport {
    let plan = plan_series(spec);
    if !plan.iter().any(|p| matches!(p, Planned::Check(_))) {
        return not_run_or_skips(&plan);
    }
    let Some(tarball) = tarball else {
        return not_run(
            &plan,
            "no source archive of this version in the checkout".into(),
        );
    };
    if !quilt_available() {
        return not_run(&plan, "quilt is not installed".into());
    }
    match run_check(&plan, read, tarball, refresh) {
        Ok(report) => report,
        Err(why) => not_run(&plan, why),
    }
}

/// The report for a plan with nothing to check: every patch skipped.
fn not_run_or_skips(plan: &[Planned]) -> CheckReport {
    if plan.is_empty() {
        return CheckReport::default();
    }
    let mut report = not_run(plan, String::new());
    report.not_run = None;
    report
}

fn run_check(
    plan: &[Planned],
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
    tarball: &Path,
    refresh: Refresh,
) -> Result<CheckReport, String> {
    let scratch = tempfile::tempdir().map_err(|e| format!("no scratch directory: {e}"))?;
    let src = scratch.path().join("src");
    let patches_dir = scratch.path().join("patches");
    std::fs::create_dir_all(&patches_dir).map_err(|e| e.to_string())?;
    // Unpack only what the patches touch: quilt reads nothing else.
    let mut targets: Vec<String> = Vec::new();
    for p in plan {
        if let Planned::Check(e) = p
            && let Some(diff) = read(&e.name)
        {
            targets.extend(crate::pipeline::patch_targets(&String::from_utf8_lossy(
                &diff,
            )));
        }
    }
    let wanted = |p: &Path| targets.iter().any(|t| p.ends_with(t));
    ap_ecosystem::sdist::extract_where(tarball, &src, wanted)
        .map_err(|e| format!("could not unpack {}: {e}", tarball.display()))?;
    let entries: Vec<_> = std::fs::read_dir(&src)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .collect();
    let root = match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => src.clone(),
    };
    let mut env = crate::proc::minimal_env(&[]);
    env.insert("QUILT_PATCHES".into(), patches_dir.display().to_string());
    env.insert("QUILT_PC".into(), ".pc".into());
    env.insert("QUILTRC".into(), "/dev/null".into());
    env.insert("LC_ALL".into(), "C".into());
    env.insert("LANG".into(), "C".into());
    let quilt = Quilt {
        root: root.clone(),
        patches: patches_dir.clone(),
        env,
        scratch_text: format!("{}/", scratch.path().display()),
    };

    let mut report = CheckReport::default();
    // The patches applied so far: the series quilt sees. A failing patch is
    // taken out of it, so the patches after it are tried without it.
    let mut applied: Vec<SeriesEntry> = Vec::new();
    for p in plan {
        let e = match p {
            Planned::Skip { name, reason } => {
                report.patches.push(PatchResult {
                    name: name.clone(),
                    level: 1,
                    status: PatchStatus::NotChecked {
                        reason: reason.clone(),
                    },
                    refreshed: None,
                });
                continue;
            }
            Planned::Check(e) => e,
        };
        let result = |status| PatchResult {
            name: e.name.clone(),
            level: e.level,
            status,
            refreshed: None,
        };
        let copied =
            read(&e.name).is_some_and(|b| std::fs::write(quilt.patches.join(&e.name), b).is_ok());
        if !copied {
            report.patches.push(result(PatchStatus::NotChecked {
                reason: "the file is not in the checkout".into(),
            }));
            continue;
        }
        let mut series = applied.clone();
        series.push(e.clone());
        std::fs::write(quilt.patches.join("series"), series_text(&series))
            .map_err(|e| e.to_string())?;
        let (ok, output) = match quilt.run(&["push"]) {
            Ok(v) => v,
            Err(why) => {
                report
                    .patches
                    .push(result(PatchStatus::NotChecked { reason: why }));
                continue;
            }
        };
        match classify_push(ok, &output) {
            PushVerdict::Clean => {
                report.patches.push(result(PatchStatus::Applies));
                applied.push(e.clone());
            }
            PushVerdict::OffsetOrFuzz { fuzz } => {
                let mut r = result(PatchStatus::AppliesWithOffsetOrFuzz {
                    fuzz,
                    output: truncate(&output, OUTPUT_LIMIT),
                });
                if fuzz && refresh == Refresh::Fuzzy {
                    let level = if e.level == 0 { "0" } else { "ab" };
                    let args = ["refresh", "-p", level, "--no-timestamps", "--no-index"];
                    if quilt.run(&args).is_ok_and(|(ok, _)| ok) {
                        r.refreshed = std::fs::read(quilt.patches.join(&e.name)).ok();
                    }
                }
                report.patches.push(r);
                applied.push(e.clone());
            }
            PushVerdict::ReverseApplied => {
                // Confirmed without fuzz: a proof made with fuzz is no proof.
                let strict = quilt
                    .run(&["push", "--fuzz=0"])
                    .map(|(ok, t)| (classify_push(ok, &t), t));
                if quilt.top().as_deref() == Some(e.name.as_str()) {
                    let _ = quilt.run(&["pop", "-f"]);
                }
                std::fs::write(quilt.patches.join("series"), series_text(&applied))
                    .map_err(|e| e.to_string())?;
                let status = match strict {
                    Ok((PushVerdict::ReverseApplied, t)) => PatchStatus::AlreadyApplied {
                        output: truncate(&t, OUTPUT_LIMIT),
                    },
                    _ => PatchStatus::Fails {
                        output: truncate(&output, OUTPUT_LIMIT),
                        rejects: String::new(),
                    },
                };
                report.patches.push(result(status));
            }
            PushVerdict::Failed => {
                // Forced, to get the rejected hunks quilt would show the agent;
                // then taken back off so the next patch sees a tree without it.
                let forced = quilt
                    .run(&["push", "-f"])
                    .map(|(_, t)| t)
                    .unwrap_or_default();
                let mut rejects = String::new();
                for file in reject_files(&forced)
                    .into_iter()
                    .chain(reject_files(&output))
                {
                    let path = root.join(&file);
                    if let Ok(body) = std::fs::read_to_string(&path)
                        && !rejects.contains(&format!("--- {file}\n"))
                    {
                        rejects.push_str(&format!("--- {file}\n{body}"));
                    }
                    let _ = std::fs::remove_file(&path);
                }
                if quilt.top().as_deref() == Some(e.name.as_str()) {
                    let _ = quilt.run(&["pop", "-f"]);
                }
                std::fs::write(quilt.patches.join("series"), series_text(&applied))
                    .map_err(|e| e.to_string())?;
                report.patches.push(result(PatchStatus::Fails {
                    output: truncate(&output, OUTPUT_LIMIT),
                    rejects: truncate(&rejects, REJECTS_LIMIT),
                }));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUSH_CLEAN: &str =
        "Applying patch good.patch\npatching file f.txt\n\nNow at patch good.patch\n";
    const PUSH_OFFSET: &str = "Applying patch off.patch\npatching file f.txt\n\
        Hunk #1 succeeded at 5 (offset 2 lines).\n\nNow at patch off.patch\n";
    const PUSH_FUZZ: &str = "Applying patch fuzz.patch\npatching file f.txt\n\
        Hunk #1 succeeded at 5 with fuzz 1 (offset -1 lines).\n\nNow at patch fuzz.patch\n";
    const PUSH_FAIL: &str = "Applying patch bad.patch\npatching file g.txt\nHunk #1 FAILED at 1.\n\
        1 out of 1 hunk FAILED -- rejects in file g.txt\n\
        Patch bad.patch does not apply (enforce with -f)\n";
    const PUSH_FORCED: &str = "Applying patch bad.patch\npatching file g.txt\nHunk #1 FAILED at 1.\n\
        1 out of 1 hunk FAILED -- saving rejects to file g.txt.rej\n\
        Applied patch bad.patch (forced; needs refresh)\n";
    const PUSH_REVERSED: &str = "Applying patch up.patch\npatching file f.txt\nHunk #1 FAILED at 1.\n\
        1 out of 1 hunk FAILED -- rejects in file f.txt\nPatch up.patch can be reverse-applied\n";

    #[test]
    fn quilt_output_is_classified() {
        assert_eq!(classify_push(true, PUSH_CLEAN), PushVerdict::Clean);
        assert_eq!(
            classify_push(true, PUSH_OFFSET),
            PushVerdict::OffsetOrFuzz { fuzz: false }
        );
        assert_eq!(
            classify_push(true, PUSH_FUZZ),
            PushVerdict::OffsetOrFuzz { fuzz: true }
        );
        assert_eq!(classify_push(false, PUSH_FAIL), PushVerdict::Failed);
        // A failed push is a failure whatever its text says.
        assert_eq!(classify_push(false, PUSH_CLEAN), PushVerdict::Failed);
        assert_eq!(
            classify_push(false, PUSH_REVERSED),
            PushVerdict::ReverseApplied
        );
        // Only a failed push can be a proof.
        assert_eq!(classify_push(true, PUSH_REVERSED), PushVerdict::Clean);
    }

    #[test]
    fn reject_files_are_read_from_both_forms() {
        assert_eq!(reject_files(PUSH_FAIL), ["g.txt.rej"]);
        assert_eq!(reject_files(PUSH_FORCED), ["g.txt.rej"]);
        assert!(reject_files(PUSH_CLEAN).is_empty());
    }

    #[test]
    fn the_series_follows_the_patch_numbers_and_levels() {
        let spec = "Name: foo\nVersion: 2.0\nPatch10: late.patch\nPatch1: %{name}-%{version}-early.patch\n\
                    Patch2: zero.patch\nPatch3: three.patch\nPatch4: %{weird}.patch\n\
                    %prep\n%autosetup -p1 -N\n%autopatch -p1 -M 1\n%patch -P 2 -p0\n%patch3 -p3\n";
        let plan = plan_series(spec);
        let checked: Vec<SeriesEntry> = plan
            .iter()
            .filter_map(|p| match p {
                Planned::Check(e) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            checked.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["foo-2.0-early.patch", "zero.patch", "late.patch"]
        );
        assert_eq!(
            series_text(&checked),
            "foo-2.0-early.patch\nzero.patch -p0\nlate.patch\n"
        );
        let skipped: Vec<(&str, &str)> = plan
            .iter()
            .filter_map(|p| match p {
                Planned::Skip { name, reason } => Some((name.as_str(), reason.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(
            skipped
                .iter()
                .any(|(n, r)| *n == "three.patch" && r.contains("-p3"))
        );
        assert!(skipped.iter().any(|(_, r)| r.contains("plain file name")));
    }

    #[test]
    fn a_patch_the_spec_never_applies_is_not_checked() {
        let spec = "Name: foo\nVersion: 1\nPatch0: a.patch\nPatch1: b.patch\n%prep\n%setup -q\n%patch1 -p1\n";
        let plan = plan_series(spec);
        assert!(matches!(&plan[0], Planned::Check(e) if e.name == "b.patch"));
        assert!(matches!(&plan[1], Planned::Skip { name, .. } if name == "a.patch"));
    }

    fn result(name: &str, status: PatchStatus) -> PatchResult {
        PatchResult {
            name: name.into(),
            level: 1,
            status,
            refreshed: None,
        }
    }

    #[test]
    fn the_agent_report_names_every_patch_with_what_to_do() {
        let report = CheckReport {
            patches: vec![
                result("good.patch", PatchStatus::Applies),
                result(
                    "fuzz.patch",
                    PatchStatus::AppliesWithOffsetOrFuzz {
                        fuzz: true,
                        output: PUSH_FUZZ.into(),
                    },
                ),
                result(
                    "bad.patch",
                    PatchStatus::Fails {
                        output: PUSH_FAIL.into(),
                        rejects: "--- g.txt.rej\n@@ -1,3 +1,3 @@\n-zwei\n+TWO\n".into(),
                    },
                ),
                result(
                    "odd.patch",
                    PatchStatus::NotChecked {
                        reason: "it applies with -p2".into(),
                    },
                ),
                result(
                    "up.patch",
                    PatchStatus::AlreadyApplied {
                        output: PUSH_REVERSED.into(),
                    },
                ),
            ],
            not_run: None,
        };
        let text = agent_report(
            &report,
            "2.0",
            &[("bad.patch".into(), "foo-1.0".into())],
            Path::new("/co/pkg"),
        );
        assert!(text.contains("2.0 sources"), "{text}");
        assert!(text.contains("good.patch: applies cleanly"), "{text}");
        assert!(text.contains("fuzz.patch: applies with fuzz"), "{text}");
        assert!(text.contains("bad.patch: FAILS"), "{text}");
        assert!(text.contains("+TWO"), "{text}");
        assert!(text.contains("`foo-1.0`, another release's tree"), "{text}");
        assert!(!text.contains("candidate for removal"), "{text}");
        assert!(
            text.contains("never removed because it does not apply"),
            "{text}"
        );
        assert!(
            text.contains("up.patch: already applied upstream"),
            "{text}"
        );
        assert!(
            !text.contains("patch -"),
            "the agent is never told about `patch`: {text}"
        );
        assert!(
            text.contains("odd.patch: not checked (it applies with -p2)"),
            "{text}"
        );
        assert!(text.contains("/co/pkg"), "{text}");
        assert_eq!(
            report.tally(),
            Tally {
                failing: 1,
                refresh: 1,
                upstream: 1
            }
        );
        assert!(!report.is_clean());
        let summary = report.summary();
        assert!(
            summary.contains("1 of 5")
                && summary.contains("odd.patch")
                && summary.contains("1 already applied upstream"),
            "{summary}"
        );
    }

    #[test]
    fn a_check_that_did_not_run_says_so_and_claims_nothing() {
        let spec = "Name: foo\nVersion: 1\nPatch0: a.patch\n%prep\n%autosetup -p1\n";
        let dir = tempfile::tempdir().unwrap();
        let report = check(spec, dir.path(), None, Refresh::No);
        assert!(report.not_run.is_some());
        assert!(matches!(
            report.patches[0].status,
            PatchStatus::NotChecked { .. }
        ));
        // Nothing failed, but the summary does not call that a success.
        assert!(
            report.summary().contains("not checked"),
            "{}",
            report.summary()
        );
    }

    #[test]
    fn the_loop_stops_on_clean_no_progress_or_rounds() {
        let t = |failing, refresh| Tally {
            failing,
            refresh,
            upstream: 0,
        };
        assert_eq!(next_round(1, 4, t(2, 0), t(0, 0), true), NextRound::Clean);
        assert_eq!(next_round(1, 4, t(2, 0), t(1, 0), false), NextRound::Again);
        assert_eq!(next_round(1, 4, t(1, 1), t(1, 0), true), NextRound::Again);
        assert_eq!(
            next_round(1, 4, t(1, 0), t(1, 0), false),
            NextRound::NoProgress
        );
        // More failures than before and no patch changed is no progress either.
        assert_eq!(
            next_round(2, 4, t(1, 0), t(2, 0), false),
            NextRound::NoProgress
        );
        assert_eq!(
            next_round(4, 4, t(2, 0), t(1, 0), false),
            NextRound::OutOfRounds
        );
    }

    #[test]
    fn bullets_are_one_line_per_patch_without_the_version() {
        assert_eq!(refresh_bullet("fix-foo.patch"), "Refresh fix-foo.patch");
        assert_eq!(
            proven_drop_bullet("fix-bar.patch"),
            "Drop fix-bar.patch: already applied upstream (reverse-applies cleanly)"
        );
    }

    #[test]
    fn a_turn_s_patch_changes_are_refreshes_and_removals() {
        let map = |items: &[(&str, Option<&str>)]| {
            items
                .iter()
                .map(|(n, c)| (n.to_string(), c.map(|c| c.as_bytes().to_vec())))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = map(&[
            ("same.patch", Some("a")),
            ("refreshed.patch", Some("b")),
            ("unlisted.patch", Some("c")),
            ("deleted.patch", Some("d")),
            ("missing.patch", None),
        ]);
        let after_names: Vec<String> = [
            "same.patch",
            "refreshed.patch",
            "deleted.patch",
            "missing.patch",
        ]
        .map(String::from)
        .to_vec();
        let after = map(&[
            ("same.patch", Some("a")),
            ("refreshed.patch", Some("B")),
            ("unlisted.patch", Some("c")),
            ("deleted.patch", None),
            ("missing.patch", None),
        ]);
        let (refreshed, removed) = diff_patches(&before, &after_names, &after);
        assert_eq!(refreshed, ["refreshed.patch"]);
        assert_eq!(removed, ["deleted.patch", "unlisted.patch"]);
    }

    #[test]
    fn truncate_cuts_on_a_char_boundary() {
        let t = truncate(&"é".repeat(10), 5);
        assert!(t.starts_with("éé") && t.contains("more bytes cut"), "{t}");
        assert_eq!(truncate("short\n", 100), "short");
    }

    /// A tarball `name-1.0.tar.gz` holding `files` under `name-1.0/`.
    fn tarball(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let path = dir.join("foo-1.0.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        for (name, body) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, format!("foo-1.0/{name}"), body.as_bytes())
                .unwrap();
        }
        ar.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn real_quilt_classifies_good_fuzzy_and_bad_patches() {
        if !quilt_available() {
            eprintln!("quilt is not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let tar = tarball(
            dir.path(),
            &[
                ("f.txt", "a\nb\nc\nd\ne\nf\ng\nh\n"),
                ("g.txt", "one\ntwo\nthree\n"),
                ("h.txt", "x\nY\nz\n"),
            ],
        );
        // Its change is already in the new sources.
        std::fs::write(
            dir.path().join("up.patch"),
            "--- a/h.txt\n+++ b/h.txt\n@@ -1,3 +1,3 @@\n x\n-y\n+Y\n z\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("good.patch"),
            "PATCH-FIX-UPSTREAM good.patch gh#1\n---\n--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("bad.patch"),
            "--- a/g.txt\n+++ b/g.txt\n@@ -1,3 +1,3 @@\n one\n-zwei\n+TWO\n three\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("fuzz.patch"),
            "Keep this header\n--- a/f.txt\n+++ b/f.txt\n@@ -6,5 +6,5 @@\n X\n f\n-g\n+G\n h\n Y\n",
        )
        .unwrap();
        let spec = "Name: foo\nVersion: 1.0\nPatch0: good.patch\nPatch1: bad.patch\nPatch2: fuzz.patch\n\
                    Patch3: up.patch\n%prep\n%autosetup -p1\n";
        let before = std::fs::read(dir.path().join("fuzz.patch")).unwrap();
        let report = check(spec, dir.path(), Some(&tar), Refresh::Fuzzy);
        assert_eq!(report.not_run, None, "{report:?}");
        let status = |n: &str| {
            report
                .patches
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("{n} missing: {report:?}"))
        };
        assert_eq!(status("good.patch").status, PatchStatus::Applies);
        match &status("bad.patch").status {
            PatchStatus::Fails { output, rejects } => {
                assert!(output.contains("FAILED"), "{output}");
                assert!(rejects.contains("+TWO"), "{rejects}");
            }
            other => panic!("{other:?}"),
        }
        // Tried on a tree without the failing patch before it.
        let fuzz = status("fuzz.patch");
        assert!(
            matches!(
                fuzz.status,
                PatchStatus::AppliesWithOffsetOrFuzz { fuzz: true, .. }
            ),
            "{fuzz:?}"
        );
        assert!(
            matches!(
                status("up.patch").status,
                PatchStatus::AlreadyApplied { .. }
            ),
            "{report:?}"
        );
        assert!(report.proves_applied("up.patch") && !report.proves_applied("bad.patch"));
        let refreshed = String::from_utf8(fuzz.refreshed.clone().unwrap()).unwrap();
        assert!(refreshed.starts_with("Keep this header"), "{refreshed}");
        assert!(refreshed.contains("+G"), "{refreshed}");
        // The checkout is never written.
        assert_eq!(
            std::fs::read(dir.path().join("fuzz.patch")).unwrap(),
            before
        );
    }
}
