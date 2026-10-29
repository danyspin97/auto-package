use super::*;

// ---------------------------------------------------------------------------
// Mode
// ---------------------------------------------------------------------------

/// What a maintenance run does to each code stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keep each code stream's version and add the upstream fix as a patch:
    /// what maintenance normally accepts. The default of `update --maintenance`.
    Backport,
    /// Update each code stream to the version `update` finds.
    Update,
    /// `lint --maintenance`: packaging fixes, the version stays.
    Lint,
}

impl Mode {
    /// `--maintenance-mode`: `backport` (the default) or `update`. `lint`
    /// runs are always [`Mode::Lint`] and take no mode.
    pub fn parse(given: Option<&str>, lint: bool) -> Result<Mode, String> {
        match (given.map(|g| g.trim().to_lowercase()), lint) {
            (None, true) => Ok(Mode::Lint),
            (Some(_), true) => Err(
                "--maintenance-mode is for update; lint --maintenance always keeps the version"
                    .to_string(),
            ),
            (None, false) => Ok(Mode::Backport),
            (Some(m), false) => match m.as_str() {
                "backport" => Ok(Mode::Backport),
                "update" => Ok(Mode::Update),
                other => Err(format!(
                    "--maintenance-mode {other}: use backport (keep the version, add the fix) or update (a version update)"
                )),
            },
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Backport => "backport",
            Mode::Update => "update",
            Mode::Lint => "lint",
        }
    }
}

/// Whether the backport flags fit the mode, refs and packages of a run.
pub fn validate_mode(
    mode: Mode,
    backports: &[String],
    backport_repo: Option<&str>,
    no_bug_ref: bool,
    packages: &[String],
) -> Result<(), String> {
    match mode {
        Mode::Backport => {
            if backports.is_empty() {
                return Err(
                    "a maintenance backport needs the fix: pass --backport PATCH|URL|COMMIT (repeatable), \
                     or --maintenance-mode update for a version update"
                        .to_string(),
                );
            }
            if no_bug_ref {
                return Err(
                    "a backport fixes a tracked bug: --no-bug-ref is refused, pass --bug bsc#NNNNNNN \
                     (and --bug CVE-YYYY-NNNN)"
                        .to_string(),
                );
            }
            if packages.len() != 1 {
                return Err(format!(
                    "a backport is one fix of one package: --only names {}",
                    if packages.is_empty() {
                        "none".to_string()
                    } else {
                        packages.join(", ")
                    }
                ));
            }
        }
        Mode::Update | Mode::Lint => {
            if !backports.is_empty() || backport_repo.is_some() {
                return Err(format!(
                    "--backport and --backport-repo are for --maintenance-mode backport, not {}",
                    mode.name()
                ));
            }
        }
    }
    if backport_repo.is_some() && backports.is_empty() {
        return Err("--backport-repo names the repository of a --backport commit id".to_string());
    }
    Ok(())
}

/// The line that says what kind of update a request is, for the plan and the
/// request description.
pub fn mode_line(mode: Mode, job: Option<&backport::Job>) -> String {
    match (mode, job) {
        (Mode::Backport, Some(job)) => {
            let patches: Vec<String> = job
                .patches
                .iter()
                .map(|p| format!("{} ({})", p.file_name, p.origin))
                .collect();
            format!(
                "Backport of {} to the version each code stream has (no version update).",
                patches.join(", ")
            )
        }
        (Mode::Backport, None) => {
            "Backport to the version each code stream has (no version update).".to_string()
        }
        (Mode::Update, _) => "Version update of the package in each code stream.".to_string(),
        (Mode::Lint, _) => "Packaging fixes; each code stream keeps its version.".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Which code streams a fix affects
// ---------------------------------------------------------------------------

/// Whether a code stream needs the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Affected {
    Affected,
    /// Left out of the run, with why (shown in the plan and the summary).
    NotAffected(String),
    /// Nobody knows: the code stream is processed (no filtering).
    Unknown,
}

/// The fix a [`StreamCheck`] is asked about.
#[derive(Debug, Clone, Copy)]
pub struct FixQuery<'a> {
    /// The bug references (`bsc#..`, `CVE-..`).
    pub refs: &'a [String],
    /// The `--backport` sources as given (empty for an update).
    pub sources: &'a [String],
}

/// The extension point for the tool that knows which code streams a fix
/// affects. Given the package and the maintained code streams (update
/// projects), it returns one verdict per code stream, in the same order. The
/// run builds every code stream that is not [`Affected::NotAffected`].
pub trait StreamCheck {
    fn check(&self, package: &str, streams: &[String], fix: &FixQuery<'_>) -> Vec<Affected>;
}

/// The default: nothing is known, every code stream is processed.
pub struct NoStreamCheck;

impl StreamCheck for NoStreamCheck {
    fn check(&self, _package: &str, streams: &[String], _fix: &FixQuery<'_>) -> Vec<Affected> {
        vec![Affected::Unknown; streams.len()]
    }
}

/// `--affected-by COMMAND`: runs `COMMAND PACKAGE CODESTREAM REFS [SOURCE...]`
/// once per code stream, without a shell and with a timeout, where REFS is the
/// comma-separated bug references and SOURCE each `--backport` argument. The
/// first line of its stdout is `affected`, `not-affected <reason>` or
/// `unknown`; anything else, a failure or a timeout counts as `unknown`.
pub struct CommandCheck {
    pub program: String,
    pub timeout: Duration,
}

/// How long `--affected-by` may take per code stream.
pub const AFFECTED_BY_TIMEOUT: Duration = Duration::from_secs(60);

/// The arguments [`CommandCheck`] passes.
pub fn affected_by_args(package: &str, stream: &str, fix: &FixQuery<'_>) -> Vec<String> {
    let mut args = vec![package.to_string(), stream.to_string(), fix.refs.join(",")];
    args.extend(fix.sources.iter().cloned());
    args
}

/// The verdict in the output of an `--affected-by` command.
pub fn parse_affected(stdout: &str, success: bool) -> Affected {
    if !success {
        return Affected::Unknown;
    }
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    match word.to_lowercase().as_str() {
        "affected" if rest.trim().is_empty() => Affected::Affected,
        "not-affected" => {
            let reason: String = rest
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            Affected::NotAffected(if reason.is_empty() {
                "the --affected-by check says it is not affected".to_string()
            } else {
                reason
            })
        }
        _ => Affected::Unknown,
    }
}

impl StreamCheck for CommandCheck {
    fn check(&self, package: &str, streams: &[String], fix: &FixQuery<'_>) -> Vec<Affected> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        streams
            .iter()
            .map(|s| {
                match proc::capture_with_timeout(
                    &self.program,
                    &affected_by_args(package, s, fix),
                    &cwd,
                    Emit::quiet(),
                    self.timeout,
                ) {
                    Ok(out) => parse_affected(&out.stdout, out.success()),
                    Err(_) => Affected::Unknown,
                }
            })
            .collect()
    }
}

/// The verdict of `check` for each of `streams`; a check that returns too few
/// leaves the rest [`Affected::Unknown`].
pub fn verdicts(
    check: &dyn StreamCheck,
    package: &str,
    streams: &[String],
    fix: &FixQuery<'_>,
) -> Vec<(String, Affected)> {
    let mut got = check.check(package, streams, fix).into_iter();
    streams
        .iter()
        .map(|s| (s.clone(), got.next().unwrap_or(Affected::Unknown)))
        .collect()
}

// ---------------------------------------------------------------------------
// Per code stream results
// ---------------------------------------------------------------------------

/// How one code stream of a package ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamOutcome {
    /// In the request; how its change went (`applied cleanly`, `updated to 1.2`).
    Ready(String),
    /// Not in the request, for a reason that is no failure (already submitted,
    /// nothing changed, declined).
    LeftOut(String),
    /// Its build or its backport failed; never sent.
    Failed(String),
    /// The `--affected-by` check says the fix is not needed there.
    NotAffected(String),
    /// Maintained in git (SLFO); not supported yet.
    GitMaintained,
}

/// One code stream's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamReport {
    /// The update project, or the `[git] ORG/PKG:BRANCH` stream.
    pub stream: String,
    pub outcome: StreamOutcome,
}

/// The summary of a package's code streams, one line each.
pub fn summary_lines(pkg: &str, reports: &[StreamReport]) -> Vec<String> {
    let ready = reports
        .iter()
        .filter(|r| matches!(r.outcome, StreamOutcome::Ready(_)))
        .count();
    let mut out = vec![format!(
        "{pkg}: {ready} of {} code stream{} ready for the maintenance request:",
        reports.len(),
        if reports.len() == 1 { "" } else { "s" }
    )];
    for r in reports {
        let what = match &r.outcome {
            StreamOutcome::Ready(n) => format!("ready, {n}"),
            StreamOutcome::LeftOut(w) => format!("left out, {w}"),
            StreamOutcome::Failed(w) => format!("FAILED, {w}"),
            StreamOutcome::NotAffected(w) => format!("not affected, {w}"),
            StreamOutcome::GitMaintained => {
                "skipped: maintained in git (SLFO), which is not supported yet".to_string()
            }
        };
        out.push(format!("  {}: {what}", r.stream));
    }
    out
}

/// The code streams of `reports` that are not in the request, with why, for
/// [`plan_lines`].
pub fn left_out(reports: &[StreamReport]) -> Vec<(String, String)> {
    reports
        .iter()
        .filter_map(|r| {
            let why = match &r.outcome {
                StreamOutcome::Ready(_) => return None,
                StreamOutcome::LeftOut(w) => w.clone(),
                StreamOutcome::Failed(w) => format!("failed: {w}"),
                StreamOutcome::NotAffected(w) => format!("not affected: {w}"),
                StreamOutcome::GitMaintained => {
                    "maintained in git (SLFO), not supported yet".to_string()
                }
            };
            Some((r.stream.clone(), why))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Build paths
// ---------------------------------------------------------------------------

/// Why a maintenance run does not add the build paths `paths` (`PROJECT/REPO`)
/// the dependency resolver chose for `pkg`, or `None` when it needs none. The
/// incident builds against the code stream only, so a package that needs
/// another project cannot be built for it as it is; the code stream fails
/// instead of building something the incident cannot reproduce.
pub fn build_path_refusal(pkg: &str, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    Some(format!(
        "{pkg} needs the build path{} {} to build, which a maintenance incident does not have: \
         not added; the code stream needs the dependency itself (a maintenance update of it first)",
        if paths.len() == 1 { "" } else { "s" },
        paths.join(", ")
    ))
}

/// The reason the dependency resolver of a maintenance run gives for not
/// adding a build path (see `DepResolver::with_frozen_paths`).
pub const FROZEN_PATHS_REASON: &str =
    "a maintenance run adds no build paths: the incident builds against the code stream only";
