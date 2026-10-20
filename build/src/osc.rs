//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ap_core::event::Event;

use crate::obs_status;
use crate::proc::{self, Captured, Emit};

mod branch;
pub use branch::*;

/// Where `osc` runs when the command is not scoped to a checkout.
fn anywhere() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Fired at most once per process the first time `rpmspec --parse` is
/// invoked, surfacing the known host-vs-target-repo macro-mismatch gap the
/// 2026-10-06 external review flagged. Behavior is unchanged — this just
/// tells the user to treat a passing `rpmspec` as a cheap syntax check, not
/// as a reproduction of what the OBS worker will do.
fn emit_host_macro_warning(emit: Emit<'_>) {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: "`rpmspec --parse` validates specs using host RPM macros, not \
                 the target OBS build repo's macros — a clean parse here does \
                 not guarantee the OBS worker will see the same expansions \
                 (e.g. `%python3_sitelib`, `%python_module`, `%suse_version`). \
                 Treat it as a syntax check, not a reproduction."
                .to_string(),
        });
    });
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run osc: {0}")]
    Launch(std::io::Error),

    #[error(transparent)]
    Process(#[from] proc::ProcessError),

    /// A launch failure or timeout, named after the osc subcommand it was for.
    #[error("{source} (during osc {cmd})")]
    Command {
        cmd: String,
        #[source]
        source: proc::ProcessError,
    },

    #[error("osc {cmd} failed (exit {code}):\n{}", osc_error_text(.stderr))]
    OscFailed {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("refusing to run the checkout's _service: {reason}")]
    ServiceRejected { reason: String },

    #[error("nothing to publish in {0}: no spec or archive found")]
    NothingToPublish(PathBuf),

    #[error("rpmspec --parse {spec} failed (exit {code}):\n{stderr}")]
    RpmspecFailed {
        spec: String,
        code: i32,
        stderr: String,
    },

    #[error(
        "refusing to run rpmspec on {spec}: it contains `{marker}`, which runs code or reads \
         files while expanding, and no sandbox (bubblewrap) is available to contain it"
    )]
    UnsandboxedMacro { spec: String, marker: String },

    #[error("osc build failed for {spec} (exit {code}); log at {log}")]
    BuildFailed {
        spec: String,
        code: i32,
        log: PathBuf,
    },

    /// Build failed for a reason that no agent fix can address.
    /// The caller should stop immediately and report the message.
    #[error("not actionable: {0}")]
    NotActionable(String),

    /// One or more RPM dependencies could not be resolved. `names` holds the
    /// raw RPM package names (e.g. `python314-puccinialin`) the caller
    /// should queue for building and retry this one against; `detail` holds
    /// the verbatim `nothing provides ...`/`(got version ...)` lines behind
    /// them, when OBS gave any — the only way to tell a genuinely missing
    /// package apart from a version constraint this spec itself wrote that
    /// the repo's one available version can never satisfy (e.g.
    /// `setuptools < 70.0.0` when only 80.9.0 is shipped).
    #[error("unresolvable RPM dependencies: {}", .names.join(", "))]
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Text that marks a failure of the machine or the network rather than of the
/// package: a dropped or refused connection, a name that did not resolve, a
/// timeout, an OBS 5xx, a full disk. Matched case-insensitively.
pub fn text_is_transient(text: &str) -> bool {
    let t = text.to_lowercase();
    const MARKS: &[&str] = &[
        "connection refused",
        "connection reset",
        "connection aborted",
        "connection closed",
        "connection timed out",
        "timed out",
        "temporary failure in name resolution",
        "name or service not known",
        "could not resolve host",
        "failed to lookup address",
        "dns error",
        "network is unreachable",
        "no route to host",
        "bad gateway",
        "service unavailable",
        "gateway time-out",
        "gateway timeout",
        "no space left on device",
        "the disk is full",
        "rate limited (429)",
        "broken pipe",
        "unexpected eof",
        "tls handshake",
    ];
    MARKS.iter().any(|m| t.contains(m))
        || contains_word(&t, "timeout")
        || http_status(text).is_some_and(|c| c == 408 || c >= 500)
}

/// Whether `word` is in `text` as a word of its own: `Error: timeout` has it,
/// `test_timeout_is_set` (a test name in a failing log) does not.
fn contains_word(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(is_word)
            && !text[at + word.len()..].chars().next().is_some_and(is_word)
    })
}

/// Marker in the message of a build whose RPMs could not be fetched after OBS
/// reported it built.
const DOWNLOAD_FAILED: &str = "succeeded but downloading the built RPMs failed";

impl Error {
    /// Whether the failure says nothing about the package and a later attempt
    /// may succeed: the network, OBS or this machine failed. A wait that gave
    /// up polling counts too (the build itself may have been fine).
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Launch(_) | Error::Process(_) | Error::Io(_) | Error::Command { .. } => {
                match self {
                    Error::Io(e) | Error::Launch(e) => io_is_transient(e),
                    Error::Process(p) | Error::Command { source: p, .. } => match p {
                        proc::ProcessError::Timeout { .. }
                        | proc::ProcessError::OutputLost { .. } => true,
                        proc::ProcessError::Io(e)
                        | proc::ProcessError::Launch { source: e, .. } => io_is_transient(e),
                    },
                    _ => false,
                }
            }
            Error::OscFailed { code, stderr, .. } => {
                // Our own wait gave up (`-1`) for another reason than a shutdown.
                *code == 28 || (*code == -1 && stderr != "interrupted") || text_is_transient(stderr)
            }
            Error::NotActionable(m) => m.contains(DOWNLOAD_FAILED),
            _ => false,
        }
    }
}

fn io_is_transient(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        e.kind(),
        K::TimedOut
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::ConnectionRefused
            | K::BrokenPipe
            | K::StorageFull
            | K::NetworkUnreachable
            | K::HostUnreachable
    ) || text_is_transient(&e.to_string())
}

/// OBS remote build status for a single arch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildStatus {
    Succeeded,
    Failed,
    Unresolvable,
    Building,
    Scheduled,
    Blocked,
    Broken,
    Disabled,
    Excluded,
    Unknown(String),
}

impl BuildStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BuildStatus::Succeeded
                | BuildStatus::Failed
                | BuildStatus::Unresolvable
                | BuildStatus::Broken
                | BuildStatus::Disabled
                | BuildStatus::Excluded
        )
    }

    /// Decode one of `osc results <project>`'s single-letter per-package
    /// status codes (see its own "Legend:" footer).
    fn from_code(c: char) -> Self {
        match c {
            '.' => BuildStatus::Succeeded,
            ' ' => BuildStatus::Disabled,
            'U' => BuildStatus::Unresolvable,
            'F' => BuildStatus::Failed,
            'B' => BuildStatus::Broken,
            'b' => BuildStatus::Blocked,
            '%' | 'd' | 'S' => BuildStatus::Building,
            's' => BuildStatus::Scheduled,
            'x' => BuildStatus::Excluded,
            other => BuildStatus::Unknown(other.to_string()),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, BuildStatus::Succeeded)
    }
}

impl std::fmt::Display for BuildStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuildStatus::Succeeded => "succeeded",
            BuildStatus::Failed => "failed",
            BuildStatus::Unresolvable => "unresolvable",
            BuildStatus::Building => "building",
            BuildStatus::Scheduled => "scheduled",
            BuildStatus::Blocked => "blocked",
            BuildStatus::Broken => "broken",
            BuildStatus::Disabled => "disabled",
            BuildStatus::Excluded => "excluded",
            BuildStatus::Unknown(s) => s.as_str(),
        };
        write!(f, "{s}")
    }
}

impl From<&str> for BuildStatus {
    fn from(s: &str) -> Self {
        match s {
            "succeeded" => BuildStatus::Succeeded,
            "failed" => BuildStatus::Failed,
            "unresolvable" => BuildStatus::Unresolvable,
            "building" => BuildStatus::Building,
            "scheduled" => BuildStatus::Scheduled,
            "blocked" => BuildStatus::Blocked,
            "broken" => BuildStatus::Broken,
            "disabled" => BuildStatus::Disabled,
            "excluded" => BuildStatus::Excluded,
            other => BuildStatus::Unknown(other.to_string()),
        }
    }
}

/// Structured classification of why a build failed.
#[derive(Debug)]
pub enum FailureKind {
    NoRepositories,
    NotWorkingCopy,
    UnresolvableDeps {
        names: Vec<String>,
        detail: Vec<String>,
    },
    BuildFailed,
}

/// Classify a failed build log into a structured failure kind.
fn classify_build_failure(log: &str) -> FailureKind {
    if log.contains("no repositories defined") {
        return FailureKind::NoRepositories;
    }
    if log.contains("is not a working copy") {
        return FailureKind::NotWorkingCopy;
    }
    let names = extract_unresolvable(log);
    if !names.is_empty() {
        let detail = extract_unresolvable_detail(log);
        return FailureKind::UnresolvableDeps { names, detail };
    }
    FailureKind::BuildFailed
}

/// The `nothing provides` requirements named on one line of a build log or of
/// OBS's `<details>` text, each without the marker and without any
/// `needed by ...` clause.
///
/// OBS's `<details>` for an `unresolvable` package joins every missing
/// requirement onto ONE line (`nothing provides python314-fastar, nothing
/// provides python313-fastar, ...`), while a build log has one per line, so a
/// line may hold several. Reading only the first, as this used to, queued
/// `python314-fastar,` (comma included) and lost everything after it.
fn nothing_provides_segments(line: &str) -> Vec<String> {
    const MARKER: &str = "nothing provides ";
    let line = line.trim();
    let line = line
        .strip_prefix("unresolvable:")
        .unwrap_or(line)
        .trim_start();
    // Only a line that starts with the phrase names requirements: one that
    // merely mentions it somewhere in prose must not count.
    let Some(mut rest) = line.strip_prefix(MARKER) else {
        return Vec::new();
    };
    let mut segments = Vec::new();
    loop {
        let (segment, next) = match rest.find(MARKER) {
            Some(i) => (&rest[..i], Some(&rest[i + MARKER.len()..])),
            None => (rest, None),
        };
        let segment = segment.trim().trim_end_matches(',').trim_end();
        let segment = segment
            .split(" needed by ")
            .next()
            .unwrap_or(segment)
            .trim();
        if !segment.is_empty() {
            segments.push(segment.to_string());
        }
        match next {
            Some(n) => rest = n,
            None => break,
        }
    }
    segments
}

/// The package names in one `nothing provides` segment. Normally just the
/// first word (version constraint dropped); a rich dependency such as
/// `(python313-tomli >= 2.0.0 if python313-base < 3.11)` also carries the
/// boolean keywords `if`, `unless`, `and`, ... which are not packages.
fn segment_package_names(segment: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &["if", "unless", "else", "and", "or", "with", "without"];
    let is_keyword = |w: &str| KEYWORDS.contains(&w);
    let words: Vec<&str> = segment.split_whitespace().collect();
    let rich = words.iter().any(|w| is_keyword(w.trim_matches(['(', ')'])));
    if !rich {
        let first = words.first().copied().unwrap_or(segment);
        let pkg = clean_unresolvable_name(first);
        return if pkg.is_empty() {
            Vec::new()
        } else {
            vec![pkg.to_string()]
        };
    }
    let mut names = Vec::new();
    for w in words {
        let w = clean_unresolvable_name(w.trim_matches(['(', ')']));
        let operator_or_version =
            w.starts_with(['<', '>', '=', '!']) || w.starts_with(|c: char| c.is_ascii_digit());
        // A version bound belongs to the previous name.
        if !w.is_empty() && !is_keyword(w) && !operator_or_version {
            names.push(w.to_string());
        }
    }
    names
}

/// Extract RPM package names from "nothing provides <pkg>" lines in a build log.
///
/// Handles both bare `nothing provides <pkg>` and `unresolvable: nothing provides <pkg>`,
/// several requirements joined onto one line (OBS's `<details>` form), strips
/// any trailing `needed by ...` clause and any version constraint, and
/// deduplicates.
fn extract_unresolvable(log: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for line in log.lines() {
        for segment in nothing_provides_segments(line) {
            for pkg in segment_package_names(&segment) {
                if seen.insert(pkg.clone()) {
                    result.push(pkg);
                }
            }
        }
    }
    result
}

/// Strip decoration OBS leaves on a "nothing provides <name>" token so the
/// result is an actual RPM name: a `(`/`)` from a rich `(A or B)` requires
/// group (e.g. `python313-duckdb)`), and a trailing `[flavor]` multibuild
/// annotation (e.g. `python314-pybind11[global]`). Left in place, these
/// aren't valid RPM or PyPI names, so `strip_rpm_prefix` and PyPI lookup
/// both miss and the dependency silently never gets queued to build.
fn clean_unresolvable_name(pkg: &str) -> &str {
    // Only an unbalanced parenthesis is decoration: the `()(64bit)` of a
    // soname capability (`libcuda.so.1()(64bit)`) is part of its name.
    let balance = |s: &str| s.matches('(').count() as isize - s.matches(')').count() as isize;
    let mut pkg = pkg;
    while pkg.starts_with('(') && balance(pkg) > 0 {
        pkg = &pkg[1..];
    }
    while pkg.ends_with(')') && balance(pkg) < 0 {
        pkg = &pkg[..pkg.len() - 1];
    }
    match pkg.rfind('[') {
        Some(idx) if pkg.ends_with(']') => &pkg[..idx],
        _ => pkg,
    }
}

/// Extract the verbatim `nothing provides ...` lines from a build log or
/// `osc results --verbose` output, together with any `(got version ...)`
/// continuation line immediately following one — unlike [`extract_unresolvable`],
/// which throws away everything but the bare package name, this keeps the
/// version constraint and the version OBS actually has, e.g.:
///   `nothing provides python314-setuptools < 70.0.0`
///   `(got version 80.9.0-3.3)`
/// That pairing is the only signal that distinguishes a genuinely missing
/// package from a version constraint the spec itself wrote that the repo's
/// one available version can never satisfy — `extract_unresolvable`'s bare
/// name alone looks identical in both cases.
fn extract_unresolvable_detail(log: &str) -> Vec<String> {
    let mut result = Vec::new();
    let lines: Vec<&str> = log.lines().collect();
    for (i, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        let bare = line
            .strip_prefix("unresolvable:")
            .unwrap_or(line)
            .trim_start();
        // The solver's other complaints name no package to queue, but they
        // are what separates "needs building" from "the spec asks for the
        // impossible", so they travel with the detail.
        if bare.starts_with("have choice for ") || bare.starts_with("conflict for providers of ") {
            result.push(line.to_string());
            continue;
        }
        let segments = nothing_provides_segments(line);
        match segments.as_slice() {
            [] => continue,
            [_] => result.push(line.to_string()),
            many => result.extend(many.iter().map(|s| format!("nothing provides {s}"))),
        }
        if let Some(next) = lines.get(i + 1) {
            let next = next.trim();
            if next.starts_with('(') {
                result.push(next.to_string());
            }
        }
    }
    result
}

/// One `have choice for <capability> needed by <package>: <provider> ...` line
/// of the solver: it names no missing package, only several that could provide
/// what a package needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    pub capability: String,
    pub needed_by: String,
    pub providers: Vec<String>,
}

/// The provider choices in `detail`.
pub fn provider_choices(detail: &[String]) -> Vec<ProviderChoice> {
    detail
        .iter()
        .filter_map(|line| {
            let line = line.trim();
            let line = line
                .strip_prefix("unresolvable:")
                .unwrap_or(line)
                .trim_start();
            let rest = line.strip_prefix("have choice for ")?;
            let (capability, rest) = rest.split_once(" needed by ")?;
            let (needed_by, providers) = rest.split_once(':')?;
            Some(ProviderChoice {
                capability: capability.trim().to_string(),
                needed_by: needed_by.trim().to_string(),
                providers: providers.split_whitespace().map(str::to_string).collect(),
            })
        })
        .collect()
}

/// A package OBS has at a version that misses a constraint a spec wrote
/// (`nothing provides <name> >= V` followed by `(got version W)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMiss {
    /// Bare package name, flavors collapsed.
    pub name: String,
    /// The version OBS has, without its release.
    pub got: String,
    /// `true` when it is newer than an upper bound or a pin allows; `false` when
    /// older than a lower bound or a pin.
    pub too_new: bool,
    /// The version the constraint names when it is a lower bound or a pin
    /// (`>= 0.66b1`, `= 0.66b1`): the release to bring the package to, so a
    /// family of packages pinned together stays in step.
    pub required: Option<String>,
}

/// A rich dependency (`nothing provides (python314-trio >= 0.25.1 with
/// python314-trio < 0.34)`) names no version OBS has, so [`version_misses`]
/// cannot see that the bound is what fails. Each such line whose package
/// `shipped` knows a version of (the bare name in, the version out) and whose
/// clauses that version misses is replaced by the plain form of the first clause
/// it misses, with the `(got version ...)` line after it; every other line is
/// kept as it was.
pub fn annotate_rich_misses(
    detail: Vec<String>,
    shipped: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out = Vec::with_capacity(detail.len());
    for line in detail {
        match rich_miss(&line, shipped) {
            Some((plain, got)) => {
                out.push(plain);
                out.push(got);
            }
            None => out.push(line),
        }
    }
    out
}

fn rich_miss(line: &str, shipped: &dyn Fn(&str) -> Option<String>) -> Option<(String, String)> {
    let line = line.trim();
    let line = line
        .strip_prefix("unresolvable:")
        .unwrap_or(line)
        .trim_start();
    let rest = line.strip_prefix("nothing provides ")?.trim_start();
    let group = rest.split(" needed by ").next()?.trim();
    let inner = group.strip_prefix('(')?.strip_suffix(')')?;
    if inner.contains(" or ") {
        return None;
    }
    let clauses: Vec<&str> = inner.split(" with ").map(str::trim).collect();
    let (name, _) = clauses.first()?.split_once(char::is_whitespace)?;
    let bare = missing_package_names(&[name.to_string()])
        .into_iter()
        .next()?;
    let got = shipped(&bare)?;
    let got_v: pep440_rs::Version = got.parse().ok()?;
    clauses.iter().find_map(|clause| {
        let (clause_name, constraint) = clause.split_once(char::is_whitespace)?;
        let Some(Some(_)) = crate::spec::rpm_constraint_miss(constraint, &got_v) else {
            return None;
        };
        Some((
            format!("nothing provides {clause_name} {}", constraint.trim()),
            format!("(got version {got}-0)"),
        ))
    })
}

/// The version misses in `detail`. A package here is not missing: too old, it
/// is brought up to date; too new, the bound in the spec is what is wrong.
pub fn version_misses(detail: &[String]) -> Vec<VersionMiss> {
    let mut out: Vec<VersionMiss> = Vec::new();
    // `osc results --verbose` puts the `(got version ...)` on the same line as
    // its `nothing provides` (`nothing provides x < 1, (got version 2-1.4)`),
    // a build log on the next one: read both as two lines.
    let detail: Vec<String> = detail
        .iter()
        .flat_map(|l| match l.find("(got version") {
            Some(at) if at > 0 => vec![
                l[..at].trim().trim_end_matches(',').trim().to_string(),
                l[at..].trim().to_string(),
            ],
            _ => vec![l.clone()],
        })
        .collect();
    for pair in detail.windows(2) {
        let (line, next) = (pair[0].trim(), pair[1].trim());
        let Some(got_text) = next.strip_prefix("(got version") else {
            continue;
        };
        let Some(rest) = line.strip_prefix("nothing provides ") else {
            continue;
        };
        let (name, constraint) = rest
            .split_once(char::is_whitespace)
            .map(|(n, c)| (n, c.trim()))
            .unwrap_or((rest, ""));
        let got = got_text
            .trim()
            .trim_end_matches(')')
            .split(['-', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let Ok(got_v) = got.parse::<pep440_rs::Version>() else {
            continue;
        };
        let Some(Some(too_new)) = crate::spec::rpm_constraint_miss(constraint, &got_v) else {
            continue;
        };
        let Some(bare) = missing_package_names(&[name.to_string()])
            .into_iter()
            .next()
        else {
            continue;
        };
        let required = ["<=", ">=", "==", "<", ">", "="]
            .iter()
            .find_map(|op| constraint.strip_prefix(op).map(|r| (*op, r.trim())))
            .filter(|(op, _)| matches!(*op, ">=" | "==" | "="))
            .map(|(_, r)| r.split(['-', ' ']).next().unwrap_or(r).to_string());
        if !out.iter().any(|m| fuzzy_key(&m.name) == fuzzy_key(&bare)) {
            out.push(VersionMiss {
                name: bare,
                got,
                too_new,
                required,
            });
        }
    }
    out
}

/// The packages in `detail` OBS has at a version older than a constraint asks
/// for.
pub fn outdated_packages(detail: &[String]) -> Vec<(String, Option<String>)> {
    version_misses(detail)
        .into_iter()
        .filter(|m| !m.too_new)
        .map(|m| (m.name, m.required))
        .collect()
}

#[cfg(test)]
mod extract_unresolvable_tests {
    use super::*;

    #[test]
    fn a_provider_choice_line_is_read() {
        let detail = vec![
            "have choice for libnccl.so.2()(64bit) needed by python314-nccl-extensions: python313-nvidia-nccl-cu13 python314-nvidia-nccl-cu13".to_string(),
        ];
        let c = provider_choices(&detail);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].capability, "libnccl.so.2()(64bit)");
        assert_eq!(c[0].needed_by, "python314-nccl-extensions");
        assert_eq!(
            c[0].providers,
            ["python313-nvidia-nccl-cu13", "python314-nvidia-nccl-cu13"]
        );
    }

    #[test]
    fn a_package_at_an_older_version_is_outdated_not_missing() {
        let detail = vec![
            "nothing provides python313-opentelemetry-instrumentation >= 0.66b1".to_string(),
            "(got version 0.66b0-1.1)".to_string(),
            "nothing provides python314-opentelemetry-instrumentation >= 0.66b1".to_string(),
            "(got version 0.66b0-1.1)".to_string(),
            "nothing provides python313-logfire".to_string(),
        ];
        assert_eq!(
            outdated_packages(&detail),
            [(
                "opentelemetry-instrumentation".to_string(),
                Some("0.66b1".to_string())
            )]
        );
    }

    /// `osc results --verbose` (no build log exists yet) joins the constraint
    /// and the version OBS has on one line; python-detect-installer looped on
    /// `uv-build < 0.10.0` against the distro's 0.11.7 because that form was
    /// not read.
    #[test]
    fn a_version_miss_on_one_line_is_read_like_one_on_two() {
        let detail = vec![
            "nothing provides python314-uv-build < 0.10.0, (got version 0.11.7-1.4)".to_string(),
            "nothing provides python313-uv-build < 0.10.0, (got version 0.11.7-1.4)".to_string(),
        ];
        let misses = version_misses(&detail);
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].name, "uv-build");
        assert_eq!(misses[0].got, "0.11.7");
        assert!(misses[0].too_new);
    }

    /// python-pycrdt: OBS says `nothing provides (python314-trio >= 0.25.1 with
    /// python314-trio < 0.34)` while the distro ships trio 0.34.0.
    #[test]
    fn a_rich_dependency_whose_bound_the_distro_misses_becomes_a_version_miss() {
        let detail = vec![
            "nothing provides (python314-trio >= 0.25.1 with python314-trio < 0.34)".to_string(),
            "nothing provides python313-other".to_string(),
        ];
        let shipped = |n: &str| (n == "trio").then(|| "0.34.0".to_string());
        let out = annotate_rich_misses(detail, &shipped);
        assert_eq!(
            out,
            [
                "nothing provides python314-trio < 0.34",
                "(got version 0.34.0-0)",
                "nothing provides python313-other",
            ]
        );
        let misses = version_misses(&out);
        assert_eq!(misses.len(), 1);
        assert!(misses[0].too_new);
        assert_eq!(misses[0].name, "trio");
        // A version that meets every clause, or one that is unknown, leaves the line.
        let met = |_: &str| Some("0.30.0".to_string());
        let line = vec![
            "nothing provides (python314-trio >= 0.25.1 with python314-trio < 0.34)".to_string(),
        ];
        assert_eq!(annotate_rich_misses(line.clone(), &met), line);
        assert_eq!(annotate_rich_misses(line.clone(), &|_: &str| None), line);
    }

    /// Regression test: a rich `(A or B)` requires group leaves a stray `)`
    /// on the last alternative's "nothing provides" line, and a multibuild
    /// flavor annotation leaves a trailing `[flavor]`. Either one turns the
    /// extracted name into something neither `strip_rpm_prefix` nor a PyPI
    /// lookup recognizes, so the dependency silently drops out of the queue
    /// instead of being built.
    #[test]
    fn strips_boolean_group_parens_and_flavor_brackets() {
        let log = "\
unresolvable: nothing provides python314-pytest-rerunfailures
unresolvable: nothing provides python313-duckdb)
nothing provides python314-pybind11[global] needed by python314-fastar
nothing provides (python313-foo
";
        let names = extract_unresolvable(log);
        assert_eq!(
            names,
            vec![
                "python314-pytest-rerunfailures",
                "python313-duckdb",
                "python314-pybind11",
                "python313-foo",
            ]
        );
    }

    /// A rich dependency's `if` is a keyword, never a package.
    #[test]
    fn rich_dependency_keywords_are_not_package_names() {
        let log = "\
nothing provides (python313-tomli >= 2.0.0 if python313-base < 3.11) needed by python313-fastapi-cli
nothing provides if python313-base < 3.11
";
        assert_eq!(
            extract_unresolvable(log),
            vec!["python313-tomli", "python313-base"]
        );
    }

    #[test]
    fn leaves_ordinary_names_untouched() {
        assert_eq!(
            clean_unresolvable_name("python314-duckdb"),
            "python314-duckdb"
        );
    }

    /// The `()(64bit)` of a soname capability is part of the name; stripping
    /// the trailing `)` turned it into something no lookup recognizes.
    #[test]
    fn keeps_the_parentheses_of_a_soname_capability() {
        assert_eq!(
            extract_unresolvable(
                "nothing provides libcuda.so.1()(64bit) needed by python313-torch"
            ),
            vec!["libcuda.so.1()(64bit)"]
        );
        assert_eq!(
            clean_unresolvable_name("pkgconfig(zlib)"),
            "pkgconfig(zlib)"
        );
    }

    /// The `<details>` of python-fastapi-cloud-cli on i586 in
    /// home:dspinella:open-webui, verbatim: every requirement on one line.
    const FASTAPI_CLOUD_CLI_DETAILS: &str = "nothing provides python314-fastar, nothing provides python313-fastar, nothing provides python314-rignore, nothing provides python313-rignore, nothing provides python314-sentry-sdk, nothing provides python313-sentry-sdk";

    /// A joined details line used to yield only `python314-fastar,` (comma
    /// attached), so five of the six names never reached the queue.
    #[test]
    fn reads_every_requirement_on_a_joined_details_line() {
        assert_eq!(
            extract_unresolvable(FASTAPI_CLOUD_CLI_DETAILS),
            vec![
                "python314-fastar",
                "python313-fastar",
                "python314-rignore",
                "python313-rignore",
                "python314-sentry-sdk",
                "python313-sentry-sdk",
            ]
        );
        let detail = extract_unresolvable_detail(FASTAPI_CLOUD_CLI_DETAILS);
        assert_eq!(detail.len(), 6);
        assert_eq!(detail[0], "nothing provides python314-fastar");
    }

    #[test]
    fn joined_requirements_keep_needed_by_and_version_out_of_the_name() {
        let line = "unresolvable: nothing provides python314-a >= 2 needed by python314-x, \
                    nothing provides python314-b needed by python314-y";
        assert_eq!(
            extract_unresolvable(line),
            vec!["python314-a", "python314-b"]
        );
        // A line that only mentions the phrase is not a requirement.
        assert!(extract_unresolvable("error: nothing provides python314-a").is_empty());
    }

    #[test]
    fn solver_conflicts_travel_with_the_detail_but_name_no_package() {
        let log = "\
unresolvable: have choice for python3-foo needed by python314-bar: python314-foo python313-foo
unresolvable: nothing provides python314-setuptools < 70.0.0
(got version 80.9.0-3.3)
conflict for providers of python314-baz needed by python314-qux
";
        assert_eq!(extract_unresolvable(log), vec!["python314-setuptools"]);
        assert_eq!(
            extract_unresolvable_detail(log),
            vec![
                "unresolvable: have choice for python3-foo needed by python314-bar: python314-foo python313-foo",
                "unresolvable: nothing provides python314-setuptools < 70.0.0",
                "(got version 80.9.0-3.3)",
                "conflict for providers of python314-baz needed by python314-qux",
            ]
        );
    }

    #[test]
    fn blockers_collapse_flavors_to_one_package_each() {
        assert_eq!(
            blockers_from_details(FASTAPI_CLOUD_CLI_DETAILS),
            vec!["fastar", "rignore", "sentry-sdk"]
        );
    }

    #[test]
    fn a_shared_library_capability_is_never_a_package_to_build() {
        let details = "nothing provides libcuda.so.1()(64bit) needed by python314-foo, \
                       nothing provides python314-bar";
        assert_eq!(blockers_from_details(details), vec!["bar"]);
        let names = vec![
            "libcuda.so.1()(64bit)".to_string(),
            "libnvidia-ml.so.1()(64bit)".to_string(),
            "/usr/bin/nvcc".to_string(),
            "python313-beautifulsoup4".to_string(),
        ];
        assert_eq!(missing_package_names(&names), vec!["beautifulsoup4"]);
    }

    #[test]
    fn capability_names_keep_their_parentheses_when_extracted() {
        assert_eq!(
            extract_unresolvable("nothing provides libcuda.so.1()(64bit) needed by python314-x"),
            vec!["libcuda.so.1()(64bit)"]
        );
        assert!(is_rpm_capability("libcuda.so.1"));
        assert!(!is_rpm_capability("python313-duckdb)"));
        assert!(!is_rpm_capability("libsodium-devel"));
    }

    #[test]
    fn blockers_skip_version_conflicts_and_interpreter_pieces() {
        let details = "nothing provides python314-setuptools < 70.0.0, \
                       nothing provides python3-devel, \
                       nothing provides python-rpm-macros, \
                       nothing provides python314-ninja, \
                       nothing provides libfoo-devel, \
                       nothing provides python313-Foo_Bar, \
                       nothing provides python314-foo-bar";
        assert_eq!(blockers_from_details(details), vec!["Foo_Bar", "foo"]);
    }

    /// The spec OBS holds for python-cuda-bindings (13.4.3-2.1), verbatim
    /// apart from the licence header: the monorepo skeleton, never finished.
    /// Its build "succeeded" and published two ~11 KB RPMs with no modules.
    const CUDA_BINDINGS_SPEC: &str = "\
Name:           python-cuda-bindings
Version:        13.4.3
Release:        0
Summary:        Python bindings for CUDA
License:        Apache-2.0
BuildRequires:  python-rpm-macros
BuildRequires:  %{python_module pip}
%python_subpackages

# This is a monorepo: all subprojects below must become %%package sections.
# TODO: add %package -n python-%{name}-cuda-bindings for cuda_bindings/
%description
Python bindings for CUDA

%prep
%autosetup -p1 -n cuda-python-13.4.3

%build
# TODO: build each subproject in dependency order

%install
# TODO: install each subproject

%check
# TODO: check each subproject

%files %{python_files}
%license LICENSE

%changelog
";

    #[test]
    fn the_unfinished_cuda_bindings_skeleton_is_recognised() {
        let reason = unfinished_spec_reason(CUDA_BINDINGS_SPEC).unwrap();
        assert!(
            reason.contains("%files lists nothing but %license/%doc"),
            "{reason}"
        );
        assert!(reason.contains("%install has no commands"), "{reason}");
    }

    #[test]
    fn a_finished_spec_is_not_flagged() {
        let spec = "\
Name:           python-foo
%description
Foo
%prep
%autosetup
%build
%pyproject_wheel
%install
%pyproject_install
%python_expand %fdupes %{buildroot}%{$python_sitelib}
%files %{python_files}
%license LICENSE
%{python_sitelib}/foo
%{python_sitelib}/foo-%{version}.dist-info
%changelog
- A TODO in the changelog is history, not a placeholder
";
        assert_eq!(unfinished_spec_reason(spec), None);
    }

    #[test]
    fn the_warning_names_the_package_the_evidence_and_the_way_out() {
        let sizes = vec![
            (
                "python313-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
                11053,
            ),
            (
                "python314-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
                10925,
            ),
        ];
        let reason = unfinished_spec_reason(CUDA_BINDINGS_SPEC).unwrap();
        let msg = empty_package_message("python-cuda-bindings", "home:u:p", &reason, &sizes);
        assert!(
            msg.contains("python-cuda-bindings is built in home:u:p"),
            "{msg}"
        );
        assert!(
            msg.contains("python314-cuda-bindings-13.4.3-2.1.x86_64.rpm: 10925 bytes"),
            "{msg}"
        );
        assert!(msg.contains("--force"), "{msg}");
    }

    #[test]
    fn only_a_succeeded_package_of_this_arch_counts_as_built() {
        let listing: std::collections::HashSet<String> =
            ["python-fastapi-cloud-cli", "python-Foo_Bar", "python-bad"]
                .into_iter()
                .map(String::from)
                .collect();
        let statuses: HashMap<String, BuildStatus> = [
            ("python-fastapi-cloud-cli", BuildStatus::Succeeded),
            ("python-Foo_Bar", BuildStatus::Succeeded),
            ("python-bad", BuildStatus::Unresolvable),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        assert_eq!(
            built_in_project(&listing, &statuses, "fastapi-cloud-cli").as_deref(),
            Some("python-fastapi-cloud-cli")
        );
        assert_eq!(
            built_in_project(&listing, &statuses, "foo-bar").as_deref(),
            Some("python-Foo_Bar")
        );
        assert_eq!(built_in_project(&listing, &statuses, "bad"), None);
        assert_eq!(built_in_project(&listing, &statuses, "nothing"), None);
    }

    #[test]
    fn missing_names_collapse_flavors_and_drop_what_is_not_built() {
        let names: Vec<String> = [
            "python313-cuda-bindings",
            "python314-cuda-bindings",
            "python3-cuda-bindings",
            "python3-devel",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(missing_package_names(&names), vec!["cuda-bindings"]);
    }

    /// Both repositories' results arrive in one document; only the one asked
    /// for may count.
    const MULTI_REPO: &str = r#"<resultlist state="bfdf69d7aa217ca87031d1aff55605f2">
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-fastapi-cloud-cli" code="unresolvable">
      <details>nothing provides python314-fastar, nothing provides python313-fastar</details>
    </status>
  </result>
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building">
    <status package="python-fastapi-cloud-cli" code="succeeded"/>
  </result>
</resultlist>"#;

    #[test]
    fn unresolvable_details_come_from_the_requested_arch_only() {
        let details = |arch| {
            unresolvable_details_from_xml(
                MULTI_REPO,
                "openSUSE_Tumbleweed",
                arch,
                "python-fastapi-cloud-cli",
            )
        };
        assert_eq!(
            details("i586").as_deref(),
            Some("nothing provides python314-fastar, nothing provides python313-fastar")
        );
        assert_eq!(details("x86_64"), None);
        assert_eq!(details("aarch64"), None);
    }
}

/// RPM package name for a Python package: `python-<name>` (lowercased).
///
/// This is only a guess at the real name for a package that does not exist
/// yet — openSUSE's actual Python packaging has no single deterministic rule
/// for how a PyPI name's `-`/`_`/`.` characters map to the RPM name: some
/// real packages convert underscores to hyphens (PyPI's `pydantic_core` is
/// `python-pydantic-core`, `mistral_common` is `python-mistral-common`),
/// while just as many keep the underscore as-is (`typing_extensions`,
/// `setuptools_scm`, `mypy_extensions`, `prompt_toolkit`, `async_timeout` are
/// all packaged with the literal underscore), and dotted names always keep
/// the dot (`ruamel.yaml`, `jaraco.classes`, `backports.zstd`) — confirmed by
/// checking real openSUSE package listings, not derivable from the PyPI name
/// alone. Never compare this against a real package list with `==`/
/// `.contains()` for an existence check — use [`fuzzy_key`] on both sides, and
/// when it matches, use the real listing's spelling, not this guess.
///
/// With `--nvidia-redist` a name of one of NVIDIA's CUDA components (the
/// `nvidia-cublas` wheel, or the `cuda-cublas` package itself) is `cuda-<short>`
/// instead: see [`crate::redist`].
pub fn rpm_name(pypi_name: &str) -> String {
    if let Some(cuda) = crate::redist::package_name(pypi_name) {
        return cuda;
    }
    format!("python-{}", pypi_name.to_lowercase())
}

/// A comparison-only key for matching two RPM/PyPI package names that might
/// spell the same project with a different `-`/`_`/`.` choice (see
/// [`rpm_name`] for why no single guess is reliable): lowercase with every
/// `-`, `_`, and `.` removed entirely, so `pydantic_core`, `pydantic-core`,
/// and `pydantic.core` all collapse to the same key. Two distinct real PyPI
/// projects differing only in separator choice are not a realistic
/// collision to worry about here.
///
/// Use this to test "does a package matching this name already exist",
/// never to construct a name to actually build or commit under — when a
/// match is found, use the real listing's own spelling instead.
pub fn fuzzy_key(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod fuzzy_key_tests {
    use super::*;

    #[test]
    fn treats_hyphen_underscore_and_dot_as_equivalent() {
        assert_eq!(
            fuzzy_key("python-pydantic-core"),
            fuzzy_key("python-pydantic_core")
        );
        assert_eq!(
            fuzzy_key("python-ruamel.yaml"),
            fuzzy_key("python-ruamel-yaml")
        );
        assert_eq!(
            fuzzy_key("Python-Backports.Zstd"),
            fuzzy_key("python-backports_zstd")
        );
    }

    #[test]
    fn distinct_names_still_produce_distinct_keys() {
        assert_ne!(
            fuzzy_key("python-pydantic-core"),
            fuzzy_key("python-pydantic")
        );
    }
}

/// Whether `s` is safe to hand to `osc` as a project or package name and to
/// join into a path: no leading `-` (would be parsed as an option), no path
/// separators or `..`, nothing but the characters OBS itself allows.
pub fn is_valid_obs_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | ':'))
        && !s.contains("..")
}

/// Attach the osc subcommand to a launch failure or timeout, which
/// `ProcessError` reports without saying which `osc` call it was.
fn during(cmd: impl Into<String>) -> impl FnOnce(proc::ProcessError) -> Error {
    move |source| Error::Command {
        cmd: cmd.into(),
        source,
    }
}

/// First non-empty line of a command's output, for one-line diagnostics.
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Whether `osc` reported a missing project, package or file on the server.
/// A bare `404` would also match unrelated digits, so the HTTP wording is
/// required; `osc` goes through urllib, which prints `HTTP Error 404: Not Found`.
fn is_not_found(text: &str) -> bool {
    // Not a bare `Not Found`: that phrase turns up in a 500 body or a message
    // about a missing file, and a package would be created over it.
    text.contains("HTTP Error 404: Not Found")
}

fn check_obs_name(what: &str, name: &str) -> Result<(), Error> {
    if is_valid_obs_name(name) {
        Ok(())
    } else {
        Err(Error::NotActionable(format!(
            "refusing to use {what} {name:?}: not a valid OBS name"
        )))
    }
}

/// Strip the `python<digits>-` or `python-` prefix from an RPM name to get
/// the underlying package name (PyPI name for Python packages, or the bare
/// name for system packages).
pub fn strip_rpm_prefix(rpm: &str) -> &str {
    // Match `python` + optional digits + `-`, e.g. `python313-`, `python3-`, `python-`.
    if let Some(rest) = rpm.strip_prefix("python") {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        if let Some(name) = rest.strip_prefix('-') {
            return name;
        }
    }
    rpm
}

/// Strip the openSUSE devel-package wrapping from a BuildRequires name so it
/// names the upstream project, not the distro packaging convention around it
/// — `libtrantor-devel` -> `trantor`, `libfoo-dev` -> `foo` (convention 27's
/// examples). The agent is told to do this itself before reporting a C/C++
/// dependency as missing, but doesn't always (the pyarrow case: a `NoChange`
/// turn left `libarrow-devel` to fall back to OBS's raw name, which then
/// canonicalized to the `arrow` monorepo and looped back onto pyarrow
/// itself). Only strips when the `-devel`/`-dev` suffix is present — a bare
/// `lib...` name with no such suffix is frequently the real upstream/runtime
/// library name and must not be touched.
pub fn strip_devel_wrapping(name: &str) -> &str {
    let Some(stripped) = name
        .strip_suffix("-devel")
        .or_else(|| name.strip_suffix("-dev"))
    else {
        return name;
    };
    stripped.strip_prefix("lib").unwrap_or(stripped)
}

#[cfg(test)]
mod strip_devel_wrapping_tests {
    use super::*;

    #[test]
    fn strips_lib_prefix_and_devel_suffix() {
        assert_eq!(strip_devel_wrapping("libtrantor-devel"), "trantor");
    }

    #[test]
    fn strips_lib_prefix_and_dev_suffix() {
        assert_eq!(strip_devel_wrapping("libfoo-dev"), "foo");
    }

    #[test]
    fn strips_devel_suffix_without_lib_prefix() {
        assert_eq!(strip_devel_wrapping("apache-arrow-devel"), "apache-arrow");
    }

    #[test]
    fn leaves_a_bare_lib_name_without_devel_suffix_untouched() {
        assert_eq!(strip_devel_wrapping("libyaml"), "libyaml");
    }

    #[test]
    fn leaves_unrelated_names_untouched() {
        assert_eq!(strip_devel_wrapping("numpy"), "numpy");
    }
}

/// Timeout for [`distro_provides`]: a local `zypper search`, not a network
/// round trip to OBS, but `--no-refresh` still has to read each configured
/// repo's on-disk solv cache, which is slow on a cold page cache.
const ZYPPER_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Parse `/etc/zypp/repos.d/*.repo` (plain INI) and return the `baseurl` of
/// every repo whose `[alias]` section does not set `enabled=0`.
///
/// Reads the config files directly instead of `zypper lr`'s table output —
/// that output wraps and column-packs under narrow terminal widths (see
/// `get_project_build_statuses`'s doc comment for a case where exactly this
/// bit a different `osc`/`zypper` table parser), and it's one less process
/// to spawn.
fn configured_repo_urls() -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir("/etc/zypp/repos.d") else {
        return Vec::new();
    };
    let mut repos = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("repo") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let mut alias: Option<String> = None;
        let mut baseurl: Option<String> = None;
        let mut enabled = true;
        for line in contents.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let (Some(a), Some(u)) = (alias.take(), baseurl.take())
                    && enabled
                {
                    repos.push((a, u));
                }
                alias = Some(name.to_string());
                baseurl = None;
                enabled = true;
                continue;
            }
            if let Some(v) = line.strip_prefix("baseurl=") {
                baseurl = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("enabled=") {
                enabled = v.trim() != "0";
            }
        }
        if let (Some(a), Some(u)) = (alias, baseurl)
            && enabled
        {
            repos.push((a, u));
        }
    }
    repos
}

/// Whether `baseurl` is a genuine openSUSE base-distribution tree rather
/// than a third-party vendor repo (`packman`, `google-cloud-cli`, …) or an
/// arbitrary OBS sub-project (`home:*`, `openSUSE:Tools`, …) that happens to
/// publish a package under a name that collides with what's being searched
/// for. Trusts two shapes:
///
///  - the plain released binary tree, no `/repositories/` segment at all
///    (`download.opensuse.org/tumbleweed/repo/oss/`,
///    `.../update/tumbleweed/`, `.../debug/tumbleweed/repo/oss/`);
///  - the base distro project published *as* an OBS project
///    (`download.opensuse.org/repositories/openSUSE:/Tumbleweed/...` or
///    `.../openSUSE:/Factory/...` or `.../openSUSE:/Leap:...`).
///
/// Any other `/repositories/<namespace>/...` is someone's personal or devel
/// project — never "the distro already has this", no matter how official
/// its packages look.
fn is_base_distro_repo(baseurl: &str) -> bool {
    let Some(path) = baseurl
        .strip_prefix("http://download.opensuse.org/")
        .or_else(|| baseurl.strip_prefix("https://download.opensuse.org/"))
    else {
        return false;
    };
    let Some(rest) = path.strip_prefix("repositories/") else {
        // No `/repositories/` segment at all: the plain released tree.
        return true;
    };
    rest.starts_with("openSUSE:/Tumbleweed/")
        || rest.starts_with("openSUSE:/Factory/")
        || rest.starts_with("openSUSE:/Leap:")
}

/// Cache of `(capability -> provided)` for the lifetime of this process.
/// `distro_provides` is called once per reported dependency, repeatedly
/// across rounds for a package stuck reporting the same name, and
/// potentially for the same system capability (`openssl`, `gcc`, …) from
/// several unrelated packages in one run — without this, each repeat spawns
/// its own `zypper` subprocess, up to [`ZYPPER_QUERY_TIMEOUT`] apiece.
static DISTRO_PROVIDES_CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// Whether the local system's configured base-distro zypper repositories
/// already provide `capability` — checked with `zypper search --provides
/// --match-exact`, i.e. the exact capability name, not a name/summary
/// substring search, restricted via `--repo` to repos [`is_base_distro_repo`]
/// trusts.
///
/// This exists because the queue-filter checks elsewhere in this crate
/// (`try_list_project_packages` / `get_project_repo_paths`, matched by
/// [`fuzzy_key`]) only ever compare against *source package names* in OBS
/// projects — and a C library's RPM package name routinely embeds its
/// SONAME version (`libpng16`, `libopenssl3`) where the bare upstream
/// project name (`libpng`, `openssl`) does not. No separator/case
/// normalization bridges that gap, so a dependency the distro has shipped
/// for years still reads as "not found anywhere" and gets queued for a
/// redundant from-source build (observed: `libpng`, already `libpng16-16`
/// / `libpng16-devel` in openSUSE Tumbleweed, got probed, cloned from
/// upstream, and queued as a brand-new from-source C package).
///
/// `zypper search --provides` matches the actual RPM `Provides:` capability
/// list, not just the package's own name, so it catches exactly this case:
/// `libpng16-16` carries a compatibility `Provides: libpng` even though its
/// package name doesn't contain the string. `--match-exact` avoids the
/// opposite failure mode — a plain substring search would also match
/// unrelated packages that merely contain `capability` somewhere in a
/// longer provides string.
///
/// Restricting to [`is_base_distro_repo`] repos matters as much as
/// `--match-exact` does: an unrestricted search also matches whatever else
/// happens to be enabled locally (a personal `home:*` project, Packman, a
/// cloud vendor's own repo, …), none of which the actual OBS build target
/// resolves against — treating one of *those* as "the distro already has
/// it" would silently and permanently drop a dependency the build can never
/// actually satisfy, which is worse than today's "queue a redundant
/// from-source build". If no local repo is recognized as a base-distro repo
/// at all, this returns `false` rather than guessing from the full,
/// untrusted repo set.
///
/// This still assumes the local machine tracks the same distribution the
/// build target does (true for this tool's own use: it runs on the same
/// openSUSE Tumbleweed the target project builds against). A stale or
/// missing local repo cache only produces a false negative — capability
/// reported as not provided — which just falls through to the existing
/// from-source-build path, so it fails open to today's behavior rather than
/// silently skipping a dependency that's actually missing.
pub fn distro_provides(capability: &str) -> bool {
    let capability = capability.trim();
    // A name an agent reported is passed to zypper as an argument: one that
    // starts with `-` would be read as an option.
    if capability.is_empty() || capability.starts_with('-') {
        return false;
    }

    if let Some(hit) = DISTRO_PROVIDES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|cache| cache.get(capability).copied())
    {
        return hit;
    }

    let trusted_aliases: Vec<String> = configured_repo_urls()
        .into_iter()
        .filter(|(_, url)| is_base_distro_repo(url))
        .map(|(alias, _)| alias)
        .collect();
    // `None` means the query itself failed (timeout, launch, ...): that is
    // not an answer, so it is neither cached nor allowed to stick for the run.
    let provided = if trusted_aliases.is_empty() {
        Some(false)
    } else {
        // `--repo` is a `search` subcommand option, not a global one — it
        // must come after `search`, not before it (zypper rejects it
        // outright, "The flag --repo is not known", otherwise).
        let mut args: Vec<String> = vec![
            "--non-interactive".into(),
            "--no-refresh".into(),
            "search".into(),
        ];
        for alias in &trusted_aliases {
            args.push("--repo".into());
            args.push(alias.clone());
        }
        args.push("--provides".into());
        args.push("--match-exact".into());
        args.push(capability.to_string());
        match proc::capture_with_timeout(
            "zypper",
            &args,
            &anywhere(),
            Emit::quiet(),
            ZYPPER_QUERY_TIMEOUT,
        ) {
            Ok(out) if out.success() => Some(true),
            // zypper's "capability not found" exit: a real answer, cacheable.
            Ok(out) if out.code == ZYPPER_CAPABILITY_NOT_FOUND => Some(false),
            Ok(out) => {
                ap_core::diag!(
                    "  zypper search for {capability}: exit {}: {}",
                    out.code,
                    first_line(&out.stderr)
                );
                None
            }
            Err(e) => {
                ap_core::diag!("  zypper search for {capability}: {e}");
                None
            }
        }
    };

    let Some(provided) = provided else {
        return false;
    };
    DISTRO_PROVIDES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(capability.to_string(), provided);
    provided
}

/// zypper's exit code for "capability not found" (`ZYPPER_EXIT_INF_CAP_NOT_FOUND`).
const ZYPPER_CAPABILITY_NOT_FOUND: i32 = 104;

#[cfg(test)]
mod distro_provides_tests {
    use super::*;

    #[test]
    fn an_empty_capability_is_never_provided() {
        // Guards against `zypper search --provides --match-exact ""`,
        // which would otherwise shell out and list unrelated packages
        // instead of cleanly reporting "no capability to check".
        assert!(!distro_provides(""));
        assert!(!distro_provides("   "));
    }

    #[test]
    fn trusts_the_plain_released_tree_and_the_base_distro_obs_project() {
        assert!(is_base_distro_repo(
            "http://download.opensuse.org/tumbleweed/repo/oss/"
        ));
        assert!(is_base_distro_repo(
            "http://download.opensuse.org/update/tumbleweed/"
        ));
        assert!(is_base_distro_repo(
            "https://download.opensuse.org/repositories/openSUSE:/Tumbleweed/standard/"
        ));
        assert!(is_base_distro_repo(
            "https://download.opensuse.org/repositories/openSUSE:/Factory/standard/"
        ));
    }

    #[test]
    fn distrusts_vendor_repos_and_arbitrary_obs_sub_projects() {
        assert!(!is_base_distro_repo(
            "https://ftp.gwdg.de/pub/linux/misc/packman/suse/openSUSE_Tumbleweed/"
        ));
        assert!(!is_base_distro_repo(
            "https://packages.cloud.google.com/yum/repos/cloud-sdk-el10-x86_64"
        ));
        assert!(!is_base_distro_repo(
            "https://download.opensuse.org/repositories/home:/someone:/project/openSUSE_Tumbleweed/"
        ));
        assert!(!is_base_distro_repo(
            "https://download.opensuse.org/repositories/openSUSE:/Tools/openSUSE_Tumbleweed"
        ));
    }
}

type RepoPaths = Vec<(String, String)>;

/// `get_project_repo_paths` answers by project, shared for a few minutes.
static REPO_PATHS_CACHE: std::sync::Mutex<Option<HashMap<String, (Instant, RepoPaths)>>> =
    std::sync::Mutex::new(None);

/// Forget a project's cached build paths: its meta was just written.
pub(crate) fn invalidate_repo_paths(project: &str) {
    if let Some(c) = REPO_PATHS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        c.remove(project);
    }
}

/// Return the `(project, repository)` pairs the OBS project's repositories
/// build against.
///
/// Parses `osc meta prj <project>` XML and extracts every
/// `<path project="..." repository="..."/>` element. These are the upstream
/// projects and repositories (e.g. `openSUSE:Factory`, repository
/// `snapshot`) whose packages are considered "already available" and do not
/// need to be built.
///
/// The repository half of each pair is deliberately not used by either
/// caller to filter further ([`select_target`]'s resolvability check and
/// the CLI's queue filter both just list every package name in
/// `path_project`, regardless of that package's build status *in* the named
/// repository) — a package whose build in the linked repo is disabled or
/// broken there is currently still treated as available here. Narrowing that
/// would need a per-repo build-status query in addition to this one; nothing
/// does that today.
///
/// A failed query is an `Err`, never an empty list: empty reads as "no build
/// paths", which widens the set of packages reported missing.
pub fn get_project_repo_paths(
    project: &str,
    emit: Emit<'_>,
) -> Result<Vec<(String, String)>, Error> {
    check_obs_name("project", project)?;
    // The build paths of a project change when someone edits its meta, not during a
    // run: one answer is shared for a few minutes. A timed-out query is tried
    // again: a single `osc meta prj` killed after 60 s (two parallel runs, a loaded
    // machine) ended the root build of open-webui with 'could not read build paths'.
    const TTL: Duration = Duration::from_secs(180);
    if let Some((at, paths)) = REPO_PATHS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(project))
        && at.elapsed() < TTL
    {
        return Ok(paths.clone());
    }
    let cmd = format!("meta prj {project}");
    let mut last: Option<Error> = None;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep_interruptible(Duration::from_secs(5 * attempt));
        }
        match proc::capture_with_timeout(
            "osc",
            &["meta", "prj", project],
            &anywhere(),
            emit,
            OSC_QUERY_TIMEOUT,
        )
        .map_err(during(cmd.clone()))
        {
            Ok(out) if out.success() => {
                let paths = parse_project_repo_paths(&out.stdout);
                REPO_PATHS_CACHE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_insert_with(Default::default)
                    .insert(project.to_string(), (Instant::now(), paths.clone()));
                return Ok(paths);
            }
            // A server answer that is not success (unknown project, no permission)
            // does not change by asking again.
            Ok(out) => {
                return Err(Error::OscFailed {
                    cmd,
                    code: out.code,
                    stderr: out.stderr,
                });
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("three attempts were made"))
}

/// Parsing half of [`get_project_repo_paths`], split out so it is testable
/// without shelling out to `osc`.
fn parse_project_repo_paths(xml: &str) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for chunk in xml.split("<path ").skip(1) {
        let tag = &chunk[..chunk.find("/>").unwrap_or(chunk.len())];
        let project_name = extract_xml_attr(tag, "project");
        let repo_name = extract_xml_attr(tag, "repository");
        if let (Some(p), Some(r)) = (project_name, repo_name) {
            let pair = (p, r);
            if !paths.contains(&pair) {
                paths.push(pair);
            }
        }
    }
    paths
}

/// What a published-binary search matches a name by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameMatch {
    Exactly,
    Containing,
}

/// The OBS `match` expression for binaries published by one of `projects`
/// (any repository: the `snapshot` repository a project builds against is not
/// searchable, but the `standard` one it is made from is) for `arch`.
fn published_binary_match(projects: &[String], name: &str, by: NameMatch, arch: &str) -> String {
    let quote = |s: &str| s.replace('\'', "");
    let name_clause = match by {
        NameMatch::Exactly => format!("@name='{}'", quote(name)),
        NameMatch::Containing => format!("contains(@name,'{}')", quote(name)),
    };
    let projects = projects
        .iter()
        .map(|p| format!("@project='{}'", quote(p)))
        .collect::<Vec<_>>()
        .join(" or ");
    // A pure-Python (or any other noarch) RPM is published as `arch="noarch"`, so
    // `@arch='x86_64'` alone never finds it: every noarch package looked "not
    // shipped", and `python313-huggingface_hub` was queued as a new package while
    // `python313-huggingface-hub` was published.
    format!(
        "{name_clause} and ({projects}) and (@arch='{0}' or @arch='noarch')",
        quote(arch)
    )
}

/// The distinct binary names in a `/search/published/binary/id` answer.
fn parse_published_binary_names(xml: &str) -> Vec<String> {
    let mut names: Vec<String> = xml
        .split("<binary ")
        .skip(1)
        .filter_map(|chunk| {
            extract_xml_attr(&chunk[..chunk.find('>').unwrap_or(chunk.len())], "name")
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

static PUBLISHED_BINARIES_CACHE: std::sync::Mutex<Option<HashMap<String, Vec<String>>>> =
    std::sync::Mutex::new(None);

/// The names of binaries the projects `project` builds against publish for
/// `arch` that match `name`. `None` when the search could not be made (no
/// answer is not "nothing published").
fn published_binaries(
    project: &str,
    arch: &str,
    name: &str,
    by: NameMatch,
    emit: Emit<'_>,
) -> Option<Vec<String>> {
    let paths = get_project_repo_paths(project, emit).ok()?;
    let mut projects: Vec<String> = paths.into_iter().map(|(p, _)| p).collect();
    projects.sort();
    projects.dedup();
    if projects.is_empty() {
        return None;
    }
    let expr = published_binary_match(&projects, name, by, arch);
    if let Some(hit) = PUBLISHED_BINARIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(&expr).cloned())
    {
        return Some(hit);
    }
    let xml = osc_api(&format!("/search/published/binary/id?match={}", enc(&expr)))?;
    // A document without a collection is not an answer.
    if !xml.contains("<collection") {
        return None;
    }
    let names = parse_published_binary_names(&xml);
    PUBLISHED_BINARIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(expr, names.clone());
    Some(names)
}

/// Whether a project `project` builds against publishes a binary package called
/// exactly `name` for `arch`. This is what a build there can install, which the
/// host's own repositories (Packman, a different snapshot) are not. `None`
/// when the search failed.
pub fn project_ships_binary(project: &str, arch: &str, name: &str, emit: Emit<'_>) -> Option<bool> {
    if name.trim().is_empty() || name.contains('\'') {
        return Some(false);
    }
    published_binaries(project, arch, name, NameMatch::Exactly, emit).map(|n| !n.is_empty())
}

/// Whether `name` is something the projects `project` builds against provide.
/// A package name is asked of what those projects publish, which is what a
/// build there can install; the host's repositories (Packman's ffmpeg, another
/// snapshot) say nothing about it. A capability (a soname, `pkgconfig(x)`) is
/// not a binary name, so the host answers it, as does a failed search.
pub fn available_to_project(project: &str, arch: &str, name: &str, emit: Emit<'_>) -> bool {
    if is_rpm_capability(name) {
        return distro_provides(name);
    }
    match project_ships_binary(project, arch, name, emit) {
        Some(shipped) => shipped,
        None => distro_provides(name),
    }
}

/// Binaries those projects publish with a name that merely contains the stem of
/// `name` (`ffmpeg-8-devel` -> `ffmpeg-8-mini-devel`, `ffmpeg-8-libavcodec-devel`):
/// what exists when the exact name does not. Empty when nothing is similar or
/// the search failed. `name` itself and debug packages are left out.
pub fn similar_published_binaries(
    project: &str,
    arch: &str,
    name: &str,
    emit: Emit<'_>,
) -> Vec<String> {
    let stem = name
        .trim_end_matches("-devel-static")
        .trim_end_matches("-devel");
    if stem.len() < 3 || stem.contains('\'') {
        return Vec::new();
    }
    let mut found: Vec<String> =
        published_binaries(project, arch, stem, NameMatch::Containing, emit).unwrap_or_default();
    found.retain(|n| {
        n != name && !n.ends_with("-debuginfo") && !n.ends_with("-debugsource") && n.contains(stem)
    });
    // The names with the same suffix as the missing one come first.
    let wants_devel = name.ends_with("-devel");
    found.sort_by_key(|n| (n.ends_with("-devel") != wants_devel, n.len()));
    found.truncate(12);
    found
}

/// The spelling the projects publish for what `name` (an RPM name such as
/// `python313-huggingface_hub`) asks for, when it is spelled differently: RPM
/// names do not follow PyPI's rule, so the distro has `huggingface-hub` but
/// `typing_extensions`, and a spec copying the PyPI name can be wrong either
/// way. The answer is the bare name, without the `python313-` prefix.
pub fn published_respelling(
    project: &str,
    arch: &str,
    name: &str,
    emit: Emit<'_>,
) -> Option<String> {
    // The search matches substrings, not across separators: ask for the
    // longest run the spellings must share.
    let term = strip_rpm_prefix(name)
        .split(['-', '_', '.'])
        .max_by_key(|part| part.len())
        .filter(|part| part.len() >= 3 && !part.contains('\''))?;
    // `contains()` is case-sensitive and the distro spells some names with capitals
    // (`python313-Brotli`, asked for as `brotli`): look for the usual variants too.
    let mut cased: Vec<String> = Vec::new();
    let mut chars = term.chars();
    let capitalized: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    for variant in [
        term.to_string(),
        term.to_lowercase(),
        capitalized,
        term.to_uppercase(),
    ] {
        if !cased.contains(&variant) {
            cased.push(variant);
        }
    }
    let mut published: Vec<String> = Vec::new();
    let mut answered = false;
    for variant in &cased {
        if let Some(names) = published_binaries(project, arch, variant, NameMatch::Containing, emit)
        {
            answered = true;
            published.extend(names);
        }
    }
    if !answered {
        return None;
    }
    published.sort();
    published.dedup();
    respelling_among(name, &published)
}

/// The respelling of `name` among `published` binary names, if one is there
/// under the same prefix and the exact spelling is not.
fn respelling_among(name: &str, published: &[String]) -> Option<String> {
    let bare = strip_rpm_prefix(name);
    let prefix = &name[..name.len() - bare.len()];
    let key = fuzzy_key(bare);
    if published.iter().any(|p| p == name) {
        return None;
    }
    let mut found: Vec<&str> = published
        .iter()
        .filter_map(|p| p.strip_prefix(prefix))
        .filter(|p| *p != bare && fuzzy_key(p) == key && strip_rpm_prefix(p) == *p)
        .collect();
    found.sort_unstable();
    found.first().map(|p| p.to_string())
}

#[cfg(test)]
mod tracked_stray_tests {
    use super::*;

    #[test]
    fn a_tracked_file_missing_from_the_checkout_is_committed_as_deleted() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".osc")).unwrap();
        fs::write(
            dir.path().join(".osc/_files"),
            r#"<directory name="python-mcp">
  <entry name="mcp-1.28.1.tar.gz" md5="a" size="1"/>
  <entry name="mcp-2.3.0.tar.gz" md5="b" size="1"/>
  <entry name="python-mcp.spec" md5="c" size="1"/>
</directory>"#,
        )
        .unwrap();
        fs::write(dir.path().join("mcp-2.3.0.tar.gz"), b"").unwrap();
        fs::write(dir.path().join("python-mcp.spec"), b"").unwrap();
        let removed = remove_tracked_strays(dir.path(), &[], Emit::quiet());
        assert_eq!(removed, ["mcp-1.28.1.tar.gz"]);
    }
}

#[cfg(test)]
mod pending_changes_tests {
    use super::*;

    const SEP: &str = "-------------------------------------------------------------------";

    fn entry(date: &str, bullet: &str) -> String {
        format!("{SEP}\n{date} - Me <me@x.org>\n\n- {bullet}\n\n")
    }

    #[test]
    fn the_entries_of_one_upload_become_one_entry_with_every_bullet() {
        let committed = entry("Mon", "Initial packaging");
        let current = format!(
            "{}{}{committed}",
            entry("Wed", "Relax the numpy bound"),
            entry("Tue", "Add a patch")
        );
        let merged = merge_pending_changes(&current, &committed).unwrap();
        assert_eq!(
            merged,
            format!(
                "{SEP}\nWed - Me <me@x.org>\n\n- Add a patch\n- Relax the numpy bound\n\n{committed}"
            )
        );
        // Merging again finds one new entry: nothing to do.
        assert!(merge_pending_changes(&merged, &committed).is_none());
    }

    #[test]
    fn a_new_package_merges_against_an_empty_committed_file() {
        let current = format!(
            "{}{}",
            entry("Tue", "Fix"),
            entry("Mon", "Initial packaging")
        );
        let merged = merge_pending_changes(&current, "").unwrap();
        assert!(merged.contains("- Initial packaging\n- Fix\n"));
        assert_eq!(merged.matches(SEP).count(), 1);
    }

    #[test]
    fn an_edited_history_is_left_alone() {
        let current = format!("{}{}", entry("Tue", "Fix"), entry("Mon", "Other"));
        assert!(merge_pending_changes(&current, &entry("Mon", "Initial")).is_none());
    }

    #[test]
    fn the_file_is_merged_against_the_committed_copy() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".osc/sources")).unwrap();
        let committed = entry("Mon", "Initial packaging");
        fs::write(dir.path().join(".osc/sources/p.changes"), &committed).unwrap();
        let current = format!("{}{}{committed}", entry("Wed", "B"), entry("Tue", "A"));
        fs::write(dir.path().join("p.changes"), current).unwrap();
        merge_pending_changes_file(dir.path());
        let text = fs::read_to_string(dir.path().join("p.changes")).unwrap();
        assert_eq!(text.matches(SEP).count(), 2);
        assert!(text.contains("- A\n- B\n"));
    }
}

#[cfg(test)]
mod published_binary_tests {
    use super::*;

    #[test]
    fn the_match_names_the_projects_and_the_arch() {
        let m = published_binary_match(
            &["openSUSE:Factory".to_string(), "devel:x".to_string()],
            "ffmpeg-8-devel",
            NameMatch::Exactly,
            "x86_64",
        );
        assert_eq!(
            m,
            "@name='ffmpeg-8-devel' and (@project='openSUSE:Factory' or @project='devel:x') and (@arch='x86_64' or @arch='noarch')"
        );
        assert!(
            published_binary_match(&["p".into()], "a'b", NameMatch::Containing, "x86_64")
                .contains("contains(@name,'ab')")
        );
    }

    /// python-torch: the Factory recipe's `_multibuild` stayed after the spec became a
    /// wheel repackage that has no flavor of its own.
    #[test]
    fn a_multibuild_the_spec_does_not_use_is_not_published() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("_multibuild"), "<multibuild/>").unwrap();
        std::fs::write(
            dir.path().join("python-x.spec"),
            "Name: python-x\n%python_subpackages\n",
        )
        .unwrap();
        let plan = publish_plan(dir.path());
        assert!(
            plan.skipped.contains(&"_multibuild".to_string()),
            "{plan:?}"
        );
        assert!(plan.files.contains(&"python-x.spec".to_string()));
        std::fs::write(
            dir.path().join("python-x.spec"),
            "Name: python-x\n%if \"%{flavor}\" == \"openmpi4\"\n%endif\n",
        )
        .unwrap();
        assert!(
            publish_plan(dir.path())
                .files
                .contains(&"_multibuild".to_string())
        );
    }

    #[test]
    fn unused_constraints_and_multibuild_are_deleted_from_a_source_recipe() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["_constraints", "_multibuild"] {
            std::fs::write(dir.path().join(f), "<x/>").unwrap();
        }
        let spec = "Name: python-x\nBuildArch: noarch\nSource0: x-1.tar.gz\n%python_subpackages\n";
        std::fs::write(dir.path().join("python-x.spec"), spec).unwrap();
        assert!(
            publish_plan(dir.path())
                .skipped
                .contains(&"_constraints".to_string())
        );
        assert_eq!(
            drop_unused_recipe_files(dir.path()),
            ["_constraints", "_multibuild"]
        );
        assert!(!dir.path().join("_constraints").exists());
        // A native build keeps its constraints; a spec that names them does too.
        std::fs::write(dir.path().join("_constraints"), "<x/>").unwrap();
        std::fs::write(
            dir.path().join("python-x.spec"),
            "Name: python-x\nSource0: x.tar.gz\n",
        )
        .unwrap();
        assert!(drop_unused_recipe_files(dir.path()).is_empty());
        std::fs::write(
            dir.path().join("python-x.spec"),
            "Name: python-x\nBuildArch: noarch\nBuildRequires: memory-constraints\n",
        )
        .unwrap();
        assert!(drop_unused_recipe_files(dir.path()).is_empty());
    }

    #[test]
    fn rpmlint_logs_are_stashed_per_package_and_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rpmlint.log"), "a: E: bad-x y\n").unwrap();
        stash_rpmlint_log(dir.path(), "python-a");
        assert!(!dir.path().join("rpmlint.log").exists());
        std::fs::write(dir.path().join("rpmlint.log"), "flavor: W: x\n").unwrap();
        stash_rpmlint_log(dir.path(), "python-a:openmpi");
        std::fs::write(dir.path().join("rpmlint.log"), "b: E: z\n").unwrap();
        stash_rpmlint_log(dir.path(), "python-ab");
        let taken = take_rpmlint_logs(dir.path(), "python-a");
        assert_eq!(taken.len(), 2, "{taken:?}");
        assert_eq!(taken[0].0, "python-a");
        assert!(take_rpmlint_logs(dir.path(), "python-a").is_empty());
        assert_eq!(rpmlint_errors(&taken[0].1), ["a: E: bad-x y"]);
        assert!(rpmlint_errors(&taken[1].1).is_empty());
        // A log with an invalid byte is still taken, and leaves the stash.
        std::fs::write(dir.path().join("rpmlint.log"), b"c.rpm: E: bad \xff\n").unwrap();
        stash_rpmlint_log(dir.path(), "python-c");
        let bad = take_rpmlint_logs(dir.path(), "python-c");
        assert_eq!(rpmlint_errors(&bad[0].1).len(), 1);
        assert!(take_rpmlint_logs(dir.path(), "python-c").is_empty());
    }

    #[test]
    fn an_empty_rpmlintrc_goes_with_its_source_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("p.spec"),
            "Name: p\nSource0: p-1.tar.gz\nSource99: p.rpmlintrc\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("p.rpmlintrc"), "").unwrap();
        assert_eq!(drop_empty_rpmlintrc(dir.path()), ["p.rpmlintrc"]);
        assert!(!dir.path().join("p.rpmlintrc").exists());
        let spec = std::fs::read_to_string(dir.path().join("p.spec")).unwrap();
        assert_eq!(spec, "Name: p\nSource0: p-1.tar.gz\n");
        // A rpmlintrc with content stays.
        std::fs::write(dir.path().join("q.rpmlintrc"), "addFilter(\"x\")\n").unwrap();
        assert!(drop_empty_rpmlintrc(dir.path()).is_empty());
        assert!(dir.path().join("q.rpmlintrc").exists());
    }

    #[test]
    fn a_wheel_spec_drops_source_recipe_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let spec = "Name: python-x\nVersion: 2\nSource0: https://h/x-2-py3-none-any.whl#/x-2-py3-none-any.whl\n";
        std::fs::write(dir.path().join("python-x.spec"), spec).unwrap();
        for f in [
            "_service",
            "_constraints",
            "x-1.tar.gz",
            "x-2-py3-none-any.whl",
            "python-x.changes",
            "vendor.tar.xz",
            "x.obsinfo",
        ] {
            std::fs::write(dir.path().join(f), "x").unwrap();
        }
        let plan = publish_plan(dir.path());
        for f in [
            "_service",
            "_constraints",
            "x-1.tar.gz",
            "vendor.tar.xz",
            "x.obsinfo",
        ] {
            assert!(plan.skipped.contains(&f.to_string()), "{f}: {plan:?}");
        }
        for f in ["python-x.spec", "python-x.changes", "x-2-py3-none-any.whl"] {
            assert!(plan.files.contains(&f.to_string()), "{f}: {plan:?}");
        }
    }

    #[test]
    fn a_source_recipe_keeps_its_service_and_named_archives() {
        let dir = tempfile::tempdir().unwrap();
        let spec = "Name: x\nVersion: 2\nSource0: x-%{version}.tar.gz\nSource1: vendor.tar.zst\n";
        std::fs::write(dir.path().join("x.spec"), spec).unwrap();
        for f in ["_service", "x-2.tar.gz", "x-1.tar.gz", "vendor.tar.zst"] {
            std::fs::write(dir.path().join(f), "x").unwrap();
        }
        let plan = publish_plan(dir.path());
        assert!(plan.files.contains(&"_service".to_string()), "{plan:?}");
        assert!(plan.files.contains(&"x-2.tar.gz".to_string()));
        assert!(plan.files.contains(&"vendor.tar.zst".to_string()));
        assert!(plan.skipped.contains(&"x-1.tar.gz".to_string()), "{plan:?}");
    }

    #[test]
    fn binary_names_are_distinct_and_sorted() {
        let xml = r#"<collection matches="3">
  <binary name="b-devel" project="p" arch="x86_64"/>
  <binary name="a" project="p" arch="x86_64"/>
  <binary name="b-devel" project="q" arch="x86_64"/>
</collection>"#;
        assert_eq!(parse_published_binary_names(xml), ["a", "b-devel"]);
        assert!(
            parse_published_binary_names("<collection matches=\"0\">\n</collection>").is_empty()
        );
    }
}

/// Does the OBS project meta list at least one `<repository>` element?
///
/// A project with no repositories cannot build anything — every `osc build`
/// against it fails immediately with `no repositories defined` (see
/// [`FailureKind::NoRepositories`]). Catching that at the top of a run gives
/// the user a clear, actionable error up front instead of a per-package
/// `NotActionable` after each queued package has already been checked out.
///
/// Returns `Ok(true)` when at least one `<repository name="...">` is present,
/// `Ok(false)` when the project meta loaded but has none, and `Err` when the
/// `osc meta prj` call itself failed (project doesn't exist, auth broken, ...).
pub fn project_has_repositories(project: &str, emit: Emit<'_>) -> Result<bool, Error> {
    check_obs_name("project", project)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["meta", "prj", project],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("meta prj {project}")))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta prj {project}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(parse_has_repository(&out.stdout))
}

/// Parsing half of [`project_has_repositories`], split out so it is testable
/// without shelling out to `osc`.
fn parse_has_repository(xml: &str) -> bool {
    xml.contains("<repository ") || xml.contains("<repository\n") || xml.contains("<repository\t")
}

/// Extract `attr="value"` from a fragment of XML attribute text.
fn extract_xml_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

static PROJECT_PACKAGES_CACHE: OnceLock<
    Mutex<HashMap<String, Arc<std::collections::HashSet<String>>>>,
> = OnceLock::new();

/// Per-project count of invalidations. A fetch that started before an
/// invalidation holds a listing that may predate the package just committed;
/// the reader compares the generation it saw before fetching and drops the
/// result if it moved, instead of caching the stale set for the whole run.
static PROJECT_PACKAGES_GENERATION: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn project_generation(project: &str) -> u64 {
    PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project)
        .copied()
        .unwrap_or(0)
}

/// Insert `pkgs` for `project` unless it was invalidated since `generation`
/// was read. The generation is checked under the cache lock, and
/// invalidation bumps it before taking that lock, so no interleaving lets a
/// stale set land after the removal.
fn insert_if_current(
    project: &str,
    generation: u64,
    pkgs: &Arc<std::collections::HashSet<String>>,
) {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if project_generation(project) == generation {
        guard.insert(project.to_owned(), Arc::clone(pkgs));
    }
}

/// Return the set of package names that exist in `project`.
///
/// The result is cached per project name so repeated calls within a process
/// skip the network round-trip — callers look names up once per dependency,
/// so without caching a single batch can mean dozens of `osc ls` round-trips.
/// Callers that successfully commit a *new* package into `project` MUST call
/// [`invalidate_project_packages_cache`] afterward: a long `auto-package
/// build` run adds packages to the same project continuously, and this
/// cache has no TTL and no other invalidation path. Without that call, the
/// very first lookup permanently freezes the answer for the rest of the
/// process — every package built after it would look like it still doesn't
/// exist to every dependent package's availability check, for the entire
/// remainder of the run, no matter how many more succeed.
///
/// Errors when the listing cannot be fetched, so a caller cannot mistake
/// "OBS did not answer" for "the package is absent".
pub fn try_list_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<Arc<std::collections::HashSet<String>>, String> {
    let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pkgs) = guard.get(project) {
            return Ok(Arc::clone(pkgs));
        }
    }

    let generation = project_generation(project);
    // A failed listing must not be cached: an empty set would make every
    // package look absent from OBS for the rest of the run. Try up to three
    // times with a short backoff, then report the error so the next call
    // tries again.
    let mut listing = fetch_project_packages(project, emit);
    for backoff in [1, 2] {
        if listing.is_ok() || crate::proc::shutting_down() {
            break;
        }
        std::thread::sleep(Duration::from_secs(backoff));
        listing = fetch_project_packages(project, Emit::quiet());
    }
    let pkgs = listing
        .map(Arc::new)
        .map_err(|e| format!("could not list packages of {project}: {e}"))?;
    insert_if_current(project, generation, &pkgs);
    Ok(pkgs)
}

/// Drop `project`'s cached package listing so the next [`try_list_project_packages`]
/// call re-fetches it. Call this right after successfully building/committing
/// a package into `project` — see [`try_list_project_packages`]'s doc comment for
/// why skipping this silently breaks every later availability check for the
/// rest of the run, not just the one for the package just built.
pub fn invalidate_project_packages_cache(project: &str) {
    // The sources changed, so the shared build results of the project may too.
    RESULTS_CACHE.invalidate(project);
    // Bump first: a fetch in flight must see the change even if it reaches
    // `insert_if_current` before the removal below.
    *PROJECT_PACKAGES_GENERATION
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(project.to_owned())
        .or_insert(0) += 1;
    if let Some(cache) = PROJECT_PACKAGES_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(project);
    }
}

#[cfg(test)]
mod project_packages_cache_tests {
    use super::*;

    /// Regression test for the bug where a long run permanently froze every
    /// package's availability at whatever the cache first saw: builds z3
    /// -solver, pydantic-core, etc. later in the same run kept looking
    /// missing to every dependent package no matter how many succeeded,
    /// because nothing ever dropped the stale entry.
    #[test]
    fn invalidate_drops_the_cached_entry() {
        let project = "ap-test-cache-invalidation-project";
        let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        cache.lock().unwrap().insert(
            project.to_string(),
            Arc::new(std::collections::HashSet::new()),
        );
        assert!(cache.lock().unwrap().contains_key(project));

        invalidate_project_packages_cache(project);

        assert!(!cache.lock().unwrap().contains_key(project));
    }

    /// A worker that read the listing before an invalidation must not put its
    /// stale set back afterwards.
    #[test]
    fn invalidation_between_fetch_and_insert_is_not_overwritten() {
        let project = "ap-test-cache-stale-insert-project";
        let generation = project_generation(project);
        // ...fetch happens here, then another worker commits and invalidates...
        invalidate_project_packages_cache(project);
        insert_if_current(project, generation, &Arc::default());
        let cache = PROJECT_PACKAGES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        assert!(!cache.lock().unwrap().contains_key(project));

        // A fetch that started after the invalidation is cached normally.
        insert_if_current(project, project_generation(project), &Arc::default());
        assert!(cache.lock().unwrap().contains_key(project));
    }

    /// A project this call never touched must not be affected — invalidation
    /// is per-project, not a blanket cache flush of every project this
    /// process has ever looked at.
    #[test]
    fn invalidate_does_not_touch_the_cache_before_anything_is_in_it() {
        // Just needs to not panic when the cache is empty or lacks the key.
        invalidate_project_packages_cache("ap-test-project-never-cached");
    }
}

/// Enumerate the files committed to an OBS package.
///
/// Returns `None` when `osc ls` fails (project/package doesn't exist, auth
/// broken) rather than panicking, same as [`cat_file`]: the update sweep
/// uses that `None` to skip the package and record a SpecFetchFailed instead
/// of killing the whole scan.
///
/// Used so the update sweep can discover the real `.spec` filename rather
/// than guess `<pkg>.spec` — AppCo's versioned-library convention gives one
/// OBS package per upstream version (`pytorch+2.4`, `pytorch+2.5`), each
/// holding a plain `<base-name>.spec` (`pytorch.spec`) that has no relation
/// to the OBS package name. The guess silently missed every one of these
/// before (Oct 6 review finding #3).
pub fn list_package_files(project: &str, pkg: &str, emit: Emit<'_>) -> Option<Vec<String>> {
    if ![project, pkg].iter().all(|n| is_valid_obs_name(n)) {
        return None;
    }
    let out = osc_query(&["ls", project, pkg], emit, &format!("ls {project}/{pkg}"))?;
    if !out.success() {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

/// Fetch a single file's contents from an OBS package without checking out.
///
/// Returns `None` when the package doesn't exist, the file isn't in it, or
/// `osc cat` otherwise fails — the update sweep uses that `None` to skip the
/// package instead of blowing up the whole run on a stray missing spec.
pub fn cat_file(project: &str, pkg: &str, filename: &str, emit: Emit<'_>) -> Option<String> {
    if ![project, pkg].iter().all(|n| is_valid_obs_name(n)) {
        return None;
    }
    let out = osc_query(
        &["cat", project, pkg, filename],
        emit,
        &format!("cat {project}/{pkg} {filename}"),
    )?;
    if !out.success() {
        return None;
    }
    Some(out.stdout)
}

fn fetch_project_packages(
    project: &str,
    emit: Emit<'_>,
) -> Result<std::collections::HashSet<String>, String> {
    if !is_valid_obs_name(project) {
        return Err(format!(
            "refusing to use project {project:?}: not a valid OBS name"
        ));
    }
    let out = proc::capture_with_timeout(
        "osc",
        &["ls", project],
        &anywhere(),
        emit,
        // A whole distro project (openSUSE:Factory) is a long listing.
        Duration::from_secs(180),
    )
    .map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(format!("osc ls exited {}: {}", out.code, out.stderr.trim()));
    }
    Ok(parse_package_listing(&out.stdout))
}

/// Package names from `osc ls <project>`, in the spelling OBS stores them
/// (OBS names are case-sensitive: `python-APScheduler`). A multibuild package
/// is listed as `<source>:<flavor>` (`apache-arrow:python-pyarrow`) and its
/// flavor is itself a package that gets built and published (python313-pyarrow),
/// so the flavor name counts as available too — otherwise a dependency Factory
/// already ships looks absent and gets queued to be built again.
fn parse_package_listing(stdout: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for name in stdout.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((_, flavor)) = name.split_once(':')
            && flavor.starts_with("python")
        {
            out.insert(flavor.to_string());
        }
        out.insert(name.to_string());
    }
    out
}

/// The name OBS lists for the package `guess` refers to, or `guess` itself
/// when nothing listed matches.
///
/// `guess` is an [`rpm_name`] guess, which is lowercase and may spell the
/// separators differently from the real package. A listed name matches when
/// its [`fuzzy_key`] equals the guess's, so `python-apscheduler` finds
/// `python-APScheduler`. Anything that checks out, builds or names a package
/// in `project` must use this spelling, not the guess: osc, the checkout
/// directory and the spec file all follow the real name.
pub fn obs_name(listing: &std::collections::HashSet<String>, guess: &str) -> String {
    if listing.contains(guess) {
        return guess.to_string();
    }
    let key = fuzzy_key(guess);
    // Several listed spellings can share a key; take the first in sort order
    // so the answer does not depend on hash iteration order.
    listing
        .iter()
        .filter(|p| fuzzy_key(p) == key)
        .min()
        .cloned()
        .unwrap_or_else(|| guess.to_string())
}

/// Create an initial `.changes` entry for a new package using `osc vc`.
///
/// Only runs when the `.changes` file does not yet exist. `osc vc` reads the
/// committer identity from `~/.config/osc/oscrc` and writes a datestamped
/// entry at the top of the file.
pub fn create_initial_changes(
    checkout_dir: &Path,
    pkg_name: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let changes_name = format!("{pkg_name}.changes");
    if checkout_dir.join(&changes_name).exists() {
        return Ok(());
    }
    add_changes_entry(checkout_dir, "Initial packaging", emit)
}

/// Add a `.changes` entry saying `message`, with `osc vc`: the committer
/// identity comes from the osc configuration, and a missing `.changes` file is
/// created. A version bump records its own entry this way, so a package
/// updated by the tool does not reach OBS (or a submit request) without one.
/// Idempotent: when the newest entry already says `message` (an earlier run
/// added it), nothing is added.
pub fn add_changes_entry(checkout_dir: &Path, message: &str, emit: Emit<'_>) -> Result<(), Error> {
    if let Ok(rd) = fs::read_dir(checkout_dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "changes"))
            .collect();
        files.sort();
        if let Some(text) = files.first().and_then(|f| fs::read_to_string(f).ok())
            && newest_changes_entry_says(&text, message)
        {
            return Ok(());
        }
    }
    let out = proc::capture("osc", &["vc", "-m", message], checkout_dir, emit)
        .map_err(during(format!("vc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("vc -m {message:?} in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    merge_pending_changes_file(checkout_dir);
    Ok(())
}

fn is_changes_separator(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// One bullet list per upload: the entries `osc vc` added to `current` since
/// `committed` (the `.changes` OBS has) become the newest entry alone, headed by
/// its header, with the bullets of all of them, oldest first. `None` when there
/// is nothing to merge: fewer than two new entries, or `current` does not end
/// with the committed text (somebody edited the old entries).
fn merge_pending_changes(current: &str, committed: &str) -> Option<String> {
    let new = current.strip_suffix(committed)?;
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in new.lines() {
        if is_changes_separator(line) {
            blocks.push((String::new(), Vec::new()));
        } else if let Some((header, body)) = blocks.last_mut() {
            if header.is_empty() {
                *header = line.to_string();
            } else {
                body.push(line);
            }
        } else if !line.trim().is_empty() {
            return None;
        }
    }
    if blocks.len() < 2 {
        return None;
    }
    let mut bullets: Vec<String> = Vec::new();
    for (_, body) in blocks.iter().rev() {
        let text = body.join("\n");
        let text = text.trim_matches('\n').trim_end();
        if !text.is_empty() && !bullets.iter().any(|b| b == text) {
            bullets.push(text.to_string());
        }
    }
    let mut out = format!(
        "{}\n{}\n\n{}\n\n",
        "-".repeat(67),
        blocks[0].0,
        bullets.join("\n")
    );
    out.push_str(committed);
    Some(out)
}

/// [`merge_pending_changes`] on the checkout's `.changes`, against the copy OBS
/// has (`.osc/sources/`; none for a file not committed yet). Best effort: a
/// file that cannot be read or written is left as `osc vc` made it.
fn merge_pending_changes_file(checkout_dir: &Path) {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "changes"))
        .collect();
    files.sort();
    let Some(file) = files.first() else {
        return;
    };
    let Ok(current) = fs::read_to_string(file) else {
        return;
    };
    let committed = file
        .file_name()
        .and_then(|n| fs::read_to_string(checkout_dir.join(".osc/sources").join(n)).ok())
        .unwrap_or_default();
    if let Some(merged) = merge_pending_changes(&current, &committed) {
        let _ = fs::write(file, merged);
    }
}

/// Whether the newest entry of a `.changes` file (the text between its first
/// two separator lines) has `message` as one of its lines, bullet or not.
fn newest_changes_entry_says(changes: &str, message: &str) -> bool {
    let is_separator = |l: &str| l.len() >= 20 && l.bytes().all(|b| b == b'-');
    let mut started = false;
    for line in changes.lines().map(str::trim) {
        if is_separator(line) {
            if started {
                return false;
            }
            started = true;
        } else if started && line.trim_start_matches(['-', '*', ' ']) == message {
            return true;
        }
    }
    false
}

/// Ensure the package exists in `project`, creating it if not.
///
/// Creates it by pushing package meta directly (`osc meta pkg ... -F -`,
/// which the server creates implicitly if it doesn't exist yet) rather than
/// `osc mkpac`. `mkpac` infers "already registered" from local working-copy
/// state — a stray directory left over from a previous, incomplete attempt,
/// or a project checkout that still remembers the package from one — and
/// happily reports success without the package ever having been created on
/// the server. That false success surfaces much later as a 404 on `osc ci`.
/// Pushing meta directly has no such local state to get confused by: the
/// server is the only thing that decides whether the package exists.
pub fn ensure_package(
    project: &str,
    pkg_name: &str,
    work_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    // `osc ls project/pkg` exits non-zero when the package does not exist.
    let check = proc::capture_with_timeout(
        "osc",
        &["ls", &format!("{project}/{pkg_name}")],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!("ls {project}/{pkg_name}")))?;

    if check.success() {
        return Ok(());
    }
    // Only a 404 means "not there yet". Any other failure (network, auth,
    // a 5xx) says nothing about existence, and pushing meta then could
    // overwrite the title/description of a package that is in fact there.
    if !is_not_found(&check.stderr) {
        return Err(Error::OscFailed {
            cmd: format!("ls {project}/{pkg_name}"),
            code: check.code,
            stderr: check.stderr,
        });
    }
    // OBS names are case-sensitive, so the failed lookup above does not rule
    // out a package that differs only in case or separators
    // (`python-apscheduler` vs. `python-APScheduler`). Creating one would
    // leave the project with two packages for one upstream, so refuse and let
    // the caller use the existing name.
    let listing = try_list_project_packages(project, emit).map_err(Error::NotActionable)?;
    if let Some(existing) = listing
        .iter()
        .filter(|p| fuzzy_key(p) == fuzzy_key(pkg_name))
        .min()
    {
        return Err(Error::NotActionable(format!(
            "{project} already has {existing}; refusing to create {pkg_name} next to it"
        )));
    }

    let meta = format!(
        "<package name=\"{}\" project=\"{}\">\n  \
         <title/>\n  <description/>\n</package>\n",
        xml_escape(pkg_name),
        xml_escape(project)
    );
    // A fresh, uniquely named file in the private directory: a name built
    // from the package in the work directory could collide with a concurrent
    // run or an existing file. Removed when `meta_file` drops.
    let write_meta = || -> std::io::Result<tempfile::NamedTempFile> {
        let mut file = tempfile::Builder::new()
            .prefix("meta-")
            .suffix(".xml")
            .tempfile_in(proc::private_dir())?;
        std::io::Write::write_all(&mut file, meta.as_bytes())?;
        Ok(file)
    };
    let meta_file = write_meta().map_err(|e| Error::OscFailed {
        cmd: format!("meta pkg {project} {pkg_name} (writing temp meta file)"),
        code: -1,
        stderr: e.to_string(),
    })?;

    let out = proc::capture_with_timeout(
        "osc",
        &[
            "meta",
            "pkg",
            project,
            pkg_name,
            "-F",
            &meta_file.path().to_string_lossy(),
        ],
        work_dir,
        emit,
        OSC_QUERY_TIMEOUT,
    );
    drop(meta_file);
    let out = out.map_err(during(format!("meta pkg {project} {pkg_name}")))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("meta pkg {project} {pkg_name} -F -"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(())
}

/// What an osc failure says, for a message: an uncaught Python exception is
/// reduced to its last lines (the traceback names osc's own source, not the
/// cause), and a full disk is called what it is, since every package that
/// downloads or checks out then fails the same way.
fn osc_error_text(stderr: &str) -> String {
    let text = match stderr.find("Traceback (most recent call last):") {
        Some(at) => {
            let lines: Vec<&str> = stderr[at..]
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect();
            lines[lines.len().saturating_sub(2)..].join("\n")
        }
        None => stderr.trim_end().to_string(),
    };
    if text.contains("No space left on device") {
        format!("{text}\nthe disk is full: free space under the work directory and run again")
    } else {
        text
    }
}

/// Check out `project/pkg_name` into `into_dir`.
///
/// Returns the path of the checkout directory, which is
/// `into_dir/<project>/<pkg_name>/`.
pub fn checkout(
    project: &str,
    pkg_name: &str,
    into_dir: &Path,
    emit: Emit<'_>,
) -> Result<PathBuf, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", pkg_name)?;
    let checkout_dir = into_dir.join(project).join(pkg_name);

    // A checkout osc never finished (killed, or out of disk space mid-download)
    // tracks nothing, and `osc up` on it cannot recover: it trips over the files
    // the half-run already wrote. It is checked out again instead.
    discard_unfinished_checkout(&checkout_dir, emit);

    // Re-use an existing checkout rather than re-cloning every run.
    if checkout_dir.exists() {
        let out = osc_up_recovering(&checkout_dir, emit)?;

        if !out.success() {
            // 404 means `ensure_package`'s meta push hasn't propagated to
            // this checkout's view of the server yet — use the directory
            // as-is rather than failing on a lag that resolves itself.
            if is_not_found(&out.stderr) {
                return Ok(checkout_dir);
            }
            return Err(Error::OscFailed {
                cmd: format!("up {project}/{pkg_name} in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }

        return Ok(checkout_dir);
    }

    // Every `osc co` into the same directory rewrites the shared
    // `<project>/.osc/_packages` file (write `.new`, rename). With several
    // packages checking out in parallel (`--jobs`, or roots added from the
    // TUI) the renames race and one fails with FileNotFoundError, so
    // serialise checkouts process-wide.
    static CHECKOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CHECKOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let out = proc::capture_with_timeout(
        "osc",
        &["co", &format!("{project}/{pkg_name}")],
        into_dir,
        emit,
        OSC_TRANSFER_TIMEOUT,
    )
    .map_err(during(format!("co {project}/{pkg_name}")))?;

    if !out.success() {
        // Whatever osc wrote before it failed would make the next run's `osc up`
        // fail on it.
        discard_unfinished_checkout(&checkout_dir, emit);
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{pkg_name}"),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(checkout_dir)
}

/// Remove `checkout_dir` when it is a checkout that was never completed: osc
/// leaves `.osc/_in_update` while it downloads and removes it at the end, and
/// `.osc/_files` lists nothing until the download has been recorded. A working
/// copy that tracks files was completed once and keeps its local changes.
fn discard_unfinished_checkout(checkout_dir: &Path, emit: Emit<'_>) {
    if !checkout_dir.join(".osc").join("_in_update").exists()
        || !tracked_files(checkout_dir).is_empty()
    {
        return;
    }
    match fs::remove_dir_all(checkout_dir) {
        Ok(()) => emit.reporter.report(&Event::Warning {
            step: emit.step,
            message: format!(
                "{} is the remains of a checkout osc never finished; checking it out again",
                checkout_dir.display()
            ),
        }),
        Err(e) => ap_core::diag!("  could not remove {}: {e}", checkout_dir.display()),
    }
}

/// Run `osc up` from `checkout_dir` to update the working copy.
///
/// Non-fatal: an already-up-to-date or brand-new checkout exits non-zero; we
/// ignore those cleanly.
pub fn pull(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = osc_up_recovering(checkout_dir, emit)?;
    if !out.success() {
        // 404 means `ensure_package`'s meta push hasn't propagated to this
        // checkout's view of the server yet — the directory is valid,
        // nothing to pull.
        if !is_benign_up_output(&out.stdout)
            && !is_benign_up_output(&out.stderr)
            && !is_not_found(&out.stdout)
            && !is_not_found(&out.stderr)
        {
            return Err(Error::OscFailed {
                cmd: format!("up in {}", checkout_dir.display()),
                code: out.code,
                stderr: out.stderr,
            });
        }
    }
    Ok(())
}

/// Whether `text` is osc's own "nothing to do" report from `osc up`: a whole
/// line that is `nothing to update` or `At revision <n>.`. Matching a line
/// rather than a substring keeps a real failure that merely quotes those
/// words (a path, a package name, a traceback) an error.
fn is_benign_up_output(text: &str) -> bool {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .peekable();
    lines.peek().is_some()
        && lines.all(|l| {
            l == "nothing to update"
                || l.strip_prefix("At revision ")
                    .and_then(|r| r.strip_suffix('.'))
                    .is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Run `osc up` in `checkout_dir`, recovering in-place from the two known
/// self-inflicted working-copy failure modes before handing the (possibly
/// still-failing) result back to the caller: a prior run that died
/// mid-`up`/`ci` leaving `.osc/` inconsistent ([`is_inconsistent_wc_error`],
/// fixed by [`repair_wc`]), and an untracked local file colliding with a
/// same-named file the server gained in the meantime
/// ([`changes_conflict_file`], fixed by moving the local one aside).
///
/// Shared by [`checkout`]'s reuse path and [`pull`] so both get the same
/// recovery — `checkout`'s "osc up" on an existing directory is exactly the
/// same command `pull` runs, and the same interrupted-run history that leaves
/// `.osc/` inconsistent or a stray untracked `.changes` file behind can be hit
/// from either entry point, not just the one a fix happened to land in first.
/// Returns the final `osc up`'s output either way — success or not — so each
/// caller can keep interpreting failure (which stderr patterns are benign,
/// whether 404 is a non-error) in its own, different way.
fn osc_up_recovering(checkout_dir: &Path, emit: Emit<'_>) -> Result<Captured, Error> {
    // Each recovery runs at most once per call, so a checkout that keeps
    // failing the same way is reported, not retried forever.
    let mut repaired = false;
    let mut moved_aside = false;
    let up = || {
        proc::capture_with_timeout("osc", &["up"], checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
            .map_err(during(format!("up in {}", checkout_dir.display())))
    };
    let mut out = up()?;
    loop {
        if out.success() {
            return Ok(out);
        }
        if !repaired && is_inconsistent_wc_error(&out.stderr) {
            repair_wc(checkout_dir, emit)?;
            repaired = true;
        } else if !moved_aside && let Some(name) = changes_conflict_file(&out.stderr) {
            // osc only raises this when `name` is untracked in this working
            // copy's `.osc/_files` (see `Package.__update`'s check against
            // `filenamelist_unvers`), so it is always safe to move aside — it
            // was never osc's file to begin with. The usual cause here is our
            // own `create_initial_changes` writing `<pkg>.changes` directly
            // before that revision's commit reached the server, and the
            // server later gaining its own file of the same name in the
            // meantime (e.g. a prior interrupted run that got further).
            // If the move fails the checkout is not usable: that is the
            // error to report, not a retry that will hit the same conflict.
            let backup = move_aside(checkout_dir, name)?;
            emit.reporter.report(&Event::Warning {
                step: emit.step,
                message: format!(
                    "osc up: untracked local '{name}' conflicted with a newer revision's \
                     file of the same name; moved it aside as '{}' and retrying",
                    backup.display()
                ),
            });
            moved_aside = true;
        } else {
            return Ok(out);
        }
        out = up()?;
    }
}

/// Move the untracked `name` out of `checkout_dir` so `osc up` can add the
/// server's file in its place, and return where it went.
///
/// The backup goes beside the checkout (`<project>/.preexisting/<pkg>/`), not
/// inside it: `osc addremove` runs on the package directory and would commit
/// a backup left there. A backup already present is never overwritten (a
/// checkout can go through this more than once), so each one gets a numbered
/// name instead.
fn move_aside(checkout_dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let pkg = checkout_dir.file_name().unwrap_or(checkout_dir.as_os_str());
    let dir = checkout_dir
        .parent()
        .unwrap_or(checkout_dir)
        .join(".preexisting")
        .join(pkg);
    fs::create_dir_all(&dir)?;
    let mut backup = dir.join(name);
    let mut n = 1;
    while backup.exists() {
        n += 1;
        backup = dir.join(format!("{name}.{n}"));
    }
    fs::rename(checkout_dir.join(name), &backup)?;
    Ok(backup)
}

/// Extract the filename osc names in its uncaught `PackageFileConflict`
/// message from `Package.__update()` (`osc/obs_scm/package.py`): `failed to
/// add file '<name>' file/dir with the same name already exists`. Raised by
/// `osc up` when a file of that name sits on disk but untracked in this
/// working copy while the server's newer revision also adds a file with the
/// same name — an unhandled exception in osc itself, not one of its usual
/// "inconsistent working copy" messages, so `repairwc` does not apply here.
///
/// Only a `.changes` file (a bare name, not a path) is returned: that is the
/// one file this tool writes into a checkout itself, so the only conflict it
/// is safe to resolve by moving the local copy aside. Any other conflict is
/// left for the caller to report.
fn changes_conflict_file(stderr: &str) -> Option<&str> {
    let name = stderr
        .split("failed to add file '")
        .nth(1)?
        .split('\'')
        .next()?;
    (name.ends_with(".changes")
        && !name.contains('/')
        && !name.contains("..")
        && !name.starts_with('-'))
    .then_some(name)
}

#[cfg(test)]
mod failing_result_tests {
    use super::*;

    const XML: &str = r#"<resultlist state="x">
  <result project="p" repository="openSUSE_Tumbleweed" arch="x86_64" code="published" state="published">
    <status package="python-mcp" code="succeeded"/>
    <status package="python-mcp:test" code="failed"/>
  </result>
  <result project="p" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-mcp" code="failed"/>
  </result>
</resultlist>"#;

    #[test]
    fn a_failed_flavor_is_the_result_whose_log_explains_the_failure() {
        let name = failing_entry_name(
            XML,
            "openSUSE_Tumbleweed",
            "x86_64",
            "python-mcp",
            &BuildStatus::Failed,
        );
        assert_eq!(name.as_deref(), Some("python-mcp:test"));
    }

    #[test]
    fn the_package_itself_wins_when_it_has_the_status() {
        let name = failing_entry_name(
            XML,
            "openSUSE_Tumbleweed",
            "i586",
            "python-mcp",
            &BuildStatus::Failed,
        );
        assert_eq!(name.as_deref(), Some("python-mcp"));
        assert_eq!(
            failing_entry_name(
                XML,
                "openSUSE_Tumbleweed",
                "x86_64",
                "python-mcp",
                &BuildStatus::Unresolvable
            ),
            None
        );
    }
}

#[cfg(test)]
mod respelling_tests {
    use super::respelling_among;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_published_spelling_replaces_the_pypi_one() {
        let published = names(&["python313-huggingface-hub", "python314-huggingface-hub"]);
        assert_eq!(
            respelling_among("python313-huggingface_hub", &published).as_deref(),
            Some("huggingface-hub")
        );
    }

    #[test]
    fn an_exact_match_or_another_prefix_is_no_respelling() {
        let published = names(&["python313-typing_extensions", "huggingface-hub"]);
        assert_eq!(
            respelling_among("python313-typing_extensions", &published),
            None
        );
        assert_eq!(
            respelling_among("python313-huggingface_hub", &published),
            None
        );
        assert_eq!(respelling_among("python313-unrelated", &published), None);
    }
}

#[cfg(test)]
mod osc_error_text_tests {
    use super::osc_error_text;

    #[test]
    fn a_traceback_is_reduced_to_the_exception_and_a_full_disk_is_named() {
        let stderr = "/x/rpms/a.rpm\nTraceback (most recent call last):\n  File \"/usr/bin/osc\", line 6, in <module>\n    o.write(buf)\nOSError: [Errno 28] No space left on device\n";
        let text = osc_error_text(stderr);
        assert!(
            text.starts_with("    o.write(buf)\nOSError: [Errno 28]"),
            "{text}"
        );
        assert!(text.contains("the disk is full"), "{text}");
        assert!(!text.contains("/usr/bin/osc"), "{text}");
        assert_eq!(osc_error_text("404 Not Found\n"), "404 Not Found");
    }
}

#[cfg(test)]
mod unfinished_checkout_tests {
    use super::*;

    fn checkout_with(files_xml: &str, in_update: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".osc")).unwrap();
        fs::write(dir.path().join(".osc/_files"), files_xml).unwrap();
        if in_update {
            fs::create_dir(dir.path().join(".osc/_in_update")).unwrap();
        }
        fs::write(dir.path().join("_service"), "").unwrap();
        dir
    }

    #[test]
    fn a_checkout_that_tracks_nothing_and_was_mid_update_is_discarded() {
        let dir = checkout_with("<directory />", true);
        discard_unfinished_checkout(dir.path(), Emit::quiet());
        assert!(!dir.path().exists());
    }

    #[test]
    fn a_completed_or_tracking_checkout_is_kept() {
        let tracking = checkout_with(
            r#"<directory name="p"><entry name="p.spec" md5="a" size="1"/></directory>"#,
            true,
        );
        discard_unfinished_checkout(tracking.path(), Emit::quiet());
        assert!(tracking.path().join("_service").exists());

        let finished = checkout_with("<directory />", false);
        discard_unfinished_checkout(finished.path(), Emit::quiet());
        assert!(finished.path().join("_service").exists());
    }
}

#[cfg(test)]
mod changes_conflict_tests {
    use super::*;

    #[test]
    fn extracts_the_filename_from_oscs_package_file_conflict_message() {
        let stderr = "osc: failed to add file 'python-foo.changes' \
                       file/dir with the same name already exists\n";
        assert_eq!(changes_conflict_file(stderr), Some("python-foo.changes"));
    }

    #[test]
    fn returns_none_for_unrelated_errors() {
        assert_eq!(changes_conflict_file("404 Not Found"), None);
    }

    #[test]
    fn a_moved_aside_changes_file_lands_outside_the_checkout_and_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("proj").join("python-foo");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(checkout.join("python-foo.changes"), "local").unwrap();

        let first = move_aside(&checkout, "python-foo.changes").unwrap();
        assert!(!first.starts_with(&checkout));
        assert!(!checkout.join("python-foo.changes").exists());
        assert_eq!(fs::read_to_string(&first).unwrap(), "local");

        fs::write(checkout.join("python-foo.changes"), "again").unwrap();
        let second = move_aside(&checkout, "python-foo.changes").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "local");
        assert_eq!(fs::read_to_string(&second).unwrap(), "again");
    }

    #[test]
    fn only_a_changes_file_is_ever_moved_aside() {
        let spec = "osc: failed to add file 'python-foo.spec' \
                    file/dir with the same name already exists\n";
        assert_eq!(changes_conflict_file(spec), None);
        let path = "osc: failed to add file 'sub/python-foo.changes' \
                    file/dir with the same name already exists\n";
        assert_eq!(changes_conflict_file(path), None);
    }
}

#[cfg(test)]
mod not_found_tests {
    use super::*;

    #[test]
    fn recognizes_osc_http_not_found_wording() {
        assert!(is_not_found("urllib: HTTP Error 404: Not Found\n"));
        assert!(is_not_found(
            "Server returned an error: HTTP Error 404: Not Found\n"
        ));
    }

    #[test]
    fn a_not_found_phrase_without_the_http_wording_is_not_a_404() {
        assert!(!is_not_found("404 Client Error: Not Found for url"));
        assert!(!is_not_found("config file Not Found, using defaults"));
        assert!(!is_not_found(
            "HTTP Error 500: Internal Server Error: Not Found in cache"
        ));
    }

    #[test]
    fn a_bare_number_containing_404_is_not_a_missing_resource() {
        assert!(!is_not_found("request id 1404 timed out"));
        assert!(!is_not_found("Skipping foo: conflict"));
    }

    /// `ensure_package` creates a package only when `osc ls` says 404; a
    /// network, auth or server failure must not read as "missing".
    #[test]
    fn other_failures_are_not_a_missing_package() {
        assert!(!is_not_found(
            "Server returned an error: HTTP Error 500: Internal Server Error"
        ));
        assert!(!is_not_found("Name or service not known"));
        assert!(!is_not_found("HTTP Error 401: Unauthorized"));
    }
}

/// Return true if `checkout_dir`'s working copy has uncommitted local
/// changes (e.g. a spec version bump from a run that crashed before `osc
/// commit`). `osc status` prints one line per changed file and nothing when
/// the working copy is clean.
pub fn has_local_changes(checkout_dir: &Path, emit: Emit<'_>) -> bool {
    let Ok(out) = proc::capture("osc", &["status"], checkout_dir, emit) else {
        return true;
    };
    if out.success() {
        return !out.stdout.trim().is_empty();
    }
    if is_inconsistent_wc_error(&out.stderr) && repair_wc(checkout_dir, emit).is_ok() {
        return proc::capture("osc", &["status"], checkout_dir, emit)
            .map(|out| !out.stdout.trim().is_empty())
            .unwrap_or(true);
    }
    // `osc status` failed for a reason other than the one recognized,
    // repairable case (a network/server hiccup, say), or `repair_wc` itself
    // failed — either way `out.stdout` is from a failed invocation and
    // almost certainly empty, which used to fall through to the same
    // "no local changes" read as an actually-clean checkout. The caller
    // uses this to gate skipping a rebuild (see the `AlreadyBuilt` fast path
    // in pipeline.rs), so treat "couldn't tell" as "has changes" — silently
    // skipping a needed rebuild is far worse than running an unnecessary one.
    true
}

/// Whether an `osc` command failed because the working copy's `.osc/`
/// metadata is inconsistent — the message `osc` itself prints when a prior
/// run died mid-`up`/`ci` and left it half-written, naming `osc repairwc .`
/// as the fix.
fn is_inconsistent_wc_error(stderr: &str) -> bool {
    stderr.contains("inconsistent state") || stderr.contains("repairwc")
}

/// Run `osc repairwc .` to fix a working copy `osc` itself reports as
/// inconsistent (see [`is_inconsistent_wc_error`]). Note this can remove
/// files from `.osc/` — exactly what `osc`'s own error message warns about —
/// so it is only ever called after that specific error, not speculatively.
pub fn repair_wc(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    let out = proc::capture("osc", &["repairwc", "."], checkout_dir, emit)
        .map_err(during(format!("repairwc in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("repairwc in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Services a checkout's `_service` may name. The agent can edit the file, and
/// a service runs arbitrary code on this machine (`refresh_obs_scm` etc. would
/// too), so anything outside the set the pipeline and OBS packaging use is
/// refused.
const ALLOWED_SERVICES: &[&str] = &[
    "tar_scm",
    "obs_scm",
    "recompress",
    "set_version",
    "cargo_vendor",
    "node_modules",
    "download_files",
    "go_modules",
    "format_spec_file",
];

/// Parameters those services take. Notably absent are anything that names a
/// command or an output directory.
const ALLOWED_SERVICE_PARAMS: &[&str] = &[
    "url",
    "scm",
    "revision",
    "versionformat",
    "versionprefix",
    "match-tag",
    "versionrewrite-pattern",
    "versionrewrite-replacement",
    "changesgenerate",
    "changesauthor",
    "subdir",
    "filename",
    "file",
    "compression",
    "src",
    "update",
    "method",
    "cargotoml",
    "no-root-manifest",
    "version",
    "basename",
    "without-version",
    "protocol",
    "host",
    "path",
    "archive",
    "disablereview",
    "strategy",
    // tar_scm / obs_scm
    "exclude",
    "include",
    "extract",
    "submodules",
    "depth",
    "history-depth",
    "parent-tag",
    "package-meta",
    // cargo_vendor
    "versioned-dirs",
    "respect-lockfile",
    // node_modules
    "input",
    "node-dir",
    "omit-optional",
    "legacy-container",
    "cpio",
    "output",
    "source-offset",
    // download_files
    "enforceupstream",
    "enforcehash",
    // set_version
    "regex",
    "parse-version",
];

/// Params whose value is a path relative to the checkout.
const SERVICE_PATH_PARAMS: &[&str] = &[
    "subdir",
    "filename",
    "file",
    "src",
    "cargotoml",
    "archive",
    "exclude",
    "include",
    "extract",
    "input",
    "node-dir",
    "cpio",
    "output",
];

/// Check one `<param>` against the allowlists. `value` is the decoded text,
/// as osc will see it.
fn validate_service_param(name: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if !ALLOWED_SERVICE_PARAMS.contains(&name) {
        return Err(format!("param `{name}` is not allowed"));
    }
    if name == "url" && !value.starts_with("https://") {
        return Err(format!("url `{value}` must be https://"));
    }
    if name == "protocol" && value != "https" {
        return Err(format!("protocol `{value}` must be https"));
    }
    if name == "revision" && value.starts_with('-') {
        return Err(format!("revision `{value}` starts with `-`"));
    }
    if SERVICE_PATH_PARAMS.contains(&name)
        && (value.starts_with('/') || value.split('/').any(|c| c == ".."))
    {
        return Err(format!("{name} `{value}` leaves the checkout"));
    }
    Ok(())
}

/// Check a `_service` file against the allowlists. `Err` names the problem.
///
/// The file is parsed as XML and the checks run on decoded values, since osc
/// decodes character references (`&#46;&#46;/`) before using them. Anything
/// the parser cannot read, any DOCTYPE, processing instruction or entity
/// other than the five predefined ones is refused.
pub(crate) fn validate_service_xml(xml: &[u8]) -> Result<(), String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let xml = std::str::from_utf8(xml).map_err(|_| "not valid UTF-8".to_string())?;
    let mut reader = Reader::from_str(xml);
    // Open elements, and the name and text so far of the <param> being read.
    let mut depth: Vec<String> = Vec::new();
    let mut param: Option<(String, String)> = None;
    let mut seen_root = false;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("malformed XML: {e}"))?;
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let tag = e.name().as_ref().to_string();
            let parent = depth.last().map(String::as_str);
            let ok = match (tag.as_str(), parent) {
                ("services", None) => !std::mem::replace(&mut seen_root, true),
                ("service", Some("services")) | ("param", Some("service")) => true,
                _ => false,
            };
            if !ok {
                return Err(format!("unexpected markup `<{tag}`"));
            }
            let mut name = None;
            for attr in e.attributes() {
                let attr = attr.map_err(|e| format!("malformed attribute: {e}"))?;
                if attr.key.as_ref() == "name" {
                    let v = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| format!("malformed attribute: {e}"))?;
                    name = Some(v.into_owned());
                }
            }
            match tag.as_str() {
                "service" => {
                    let name = name.ok_or("a <service> without a name")?;
                    if !ALLOWED_SERVICES.contains(&name.as_str()) {
                        return Err(format!("service `{name}` is not allowed"));
                    }
                }
                "param" => {
                    let name = name.ok_or("a <param> without a name")?;
                    if empty {
                        validate_service_param(&name, "")?;
                    } else {
                        param = Some((name, String::new()));
                    }
                }
                _ => {}
            }
            if !empty {
                depth.push(tag);
            }
            continue;
        }
        match event {
            Event::End(_) => {
                if depth.pop().as_deref() == Some("param")
                    && let Some((name, value)) = param.take()
                {
                    validate_service_param(&name, &value)?;
                }
            }
            Event::Text(t) => {
                if let Some((_, value)) = param.as_mut() {
                    value.push_str(&t);
                }
            }
            Event::CData(t) => {
                if let Some((_, value)) = param.as_mut() {
                    value.push_str(&t);
                }
            }
            Event::GeneralRef(r) => {
                let ch = match r.resolve_char_ref() {
                    Ok(Some(c)) => c,
                    Ok(None) => match &*r {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        other => return Err(format!("entity `&{other};` is not allowed")),
                    },
                    Err(e) => return Err(format!("malformed reference: {e}")),
                };
                if let Some((_, value)) = param.as_mut() {
                    value.push(ch);
                }
            }
            Event::DocType(_) => return Err("a DOCTYPE is not allowed".to_string()),
            Event::PI(_) => return Err("processing instructions are not allowed".to_string()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !depth.is_empty() {
        return Err("unclosed element".to_string());
    }
    Ok(())
}

/// Run `osc service ra` from `checkout_dir` to fetch sources and vendor Cargo deps.
///
/// The `_service` file is validated first (the agent may have edited it) and
/// the service runs with a minimal environment, without publishing tokens.
pub fn run_service(checkout_dir: &Path, emit: Emit<'_>) -> Result<(), Error> {
    // Fail closed: only a missing file skips validation.
    match fs::read(checkout_dir.join("_service")) {
        Ok(xml) => {
            validate_service_xml(&xml).map_err(|reason| Error::ServiceRejected { reason })?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error::ServiceRejected {
                reason: format!("cannot read _service: {e}"),
            });
        }
    }
    let out = proc::capture_with_timeout_env(
        "osc",
        &["service", "ra"],
        checkout_dir,
        emit,
        proc::DEFAULT_CAPTURE_TIMEOUT,
        // osc reads its credentials from the desktop keyring at start-up, which
        // it reaches through the session bus: without the address it dies
        // with "Unable to initialize SecretService" before any service runs.
        Some(&proc::minimal_env(&["DBUS_SESSION_BUS_ADDRESS"])),
    )
    .map_err(during(format!("service ra in {}", checkout_dir.display())))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("service ra in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Macro forms that run a program or read a file while `rpmspec` expands them.
/// The spec may come from upstream or from the agent, so these are only
/// expanded inside the sandbox.
const CODE_MACRO_MARKERS: &[&str] = &["%(", "%{lua:", "%lua", "%{expand:", "%{load:", "%{getenv:"];

/// The first code-running macro form in `text`, if any.
fn code_macro_marker(text: &str) -> Option<&'static str> {
    CODE_MACRO_MARKERS
        .iter()
        .copied()
        .find(|m| text.contains(m))
}

/// Whether `rpmspec` can run inside bubblewrap here. Probed once: `bwrap`
/// installed is not enough, unprivileged user namespaces are often disabled.
fn rpmspec_sandbox_works() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| {
        let mut args = sandbox_prefix(None);
        args.extend(["rpmspec".to_string(), "--version".to_string()]);
        proc::capture("bwrap", &args, &anywhere(), Emit::quiet()).is_ok_and(|out| out.success())
    })
}

/// bubblewrap options for one `rpmspec` run, up to the program name. The
/// system is read-only, the home directory is not mounted, the environment is
/// cleared, and there is no network. `spec_dir`, when given, is the only user
/// directory mounted, read-only.
fn sandbox_prefix(spec_dir: Option<&str>) -> Vec<String> {
    let mut args = ap_core::sandbox::system_ro_binds();
    args.extend(ap_core::sandbox::etc_ro_binds());
    args.extend(
        [
            // rpm's own configuration, which the shared list does not carry.
            "--ro-bind-try",
            "/etc/rpm",
            "/etc/rpm",
            "--ro-bind-try",
            "/etc/rpmrc",
            "/etc/rpmrc",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--unshare-net",
            "--unshare-pid",
            "--unshare-ipc",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "LANG",
            "C.UTF-8",
        ]
        .map(String::from),
    );
    let dir = spec_dir.unwrap_or("/tmp");
    if spec_dir.is_some() {
        args.extend(["--ro-bind", dir, dir].map(String::from));
    }
    args.extend(["--chdir", dir].map(String::from));
    args
}

/// Run `rpmspec --parse` on a spec file and check for syntax errors.
///
/// Judges on exit status only (warnings in stderr are not treated as failures).
/// Returns the expanded spec on success (stdout from `rpmspec --parse`).
///
/// **Caveat (flagged by the 2026-10-06 external review):** `rpmspec` expands
/// macros using the **host** RPM configuration, not the target build repo's
/// configuration. A spec that `rpmspec --parse`s cleanly on this machine can
/// still fail to parse (or expand to different Requires / BuildRequires) on
/// the OBS build worker if the host's macro set diverges from the target
/// distro's — `%python3_sitelib`, `%python_module`, `%suse_version` and the
/// like are all host-local here. The agent still relies on this as a cheap
/// pre-flight syntax check; the one-shot [`emit_host_macro_warning`] call
/// below surfaces the gap to the user. A durable fix would require
/// containerized validation against the target repo's rpm-config.
///
/// Expanding a spec can run code (`%(...)`, `%{lua:...}`), so it runs inside
/// bubblewrap when that works. Without a sandbox, a spec containing such a
/// macro is refused rather than expanded on the host.
pub fn rpmspec_check(spec_path: &Path, emit: Emit<'_>) -> Result<String, Error> {
    emit_host_macro_warning(emit);

    let spec = spec_path.to_string_lossy().into_owned();
    let out = if rpmspec_sandbox_works() {
        let dir = spec_path.parent().unwrap_or(Path::new("/"));
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let file = dir.join(spec_path.file_name().unwrap_or_default());
        let mut args = sandbox_prefix(Some(&dir.to_string_lossy()));
        let file = file.to_string_lossy().into_owned();
        args.extend(["rpmspec".to_string(), "--parse".to_string(), file]);
        proc::capture("bwrap", &args, &anywhere(), emit)?
    } else {
        let text = fs::read(spec_path).unwrap_or_default();
        if let Some(marker) = code_macro_marker(&String::from_utf8_lossy(&text)) {
            return Err(Error::UnsandboxedMacro {
                spec,
                marker: marker.to_string(),
            });
        }
        proc::capture("rpmspec", &["--parse", &spec], &anywhere(), emit)?
    };

    if !out.success() {
        return Err(Error::RpmspecFailed {
            spec: spec_path.display().to_string(),
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(out.stdout)
}

/// What a checkout publishes to OBS, split by [`publish_plan`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PublishPlan {
    /// Checkout-relative names to stage and commit.
    pub files: Vec<String>,
    /// Names present in the checkout that are not part of the package.
    pub skipped: Vec<String>,
}

/// Decide which files of a checkout belong in the commit.
///
/// Only the spec, the archives, the service files, the changelog and rpmlint
/// config, and the quilt patches are published: a patch when `series` or the
/// spec names it. A file a `Source:` or `Patch:` line of the spec names is
/// published whatever its extension (a `LICENSE` fetched from the upstream
/// tag): `%prep` fails without it. Anything else in the directory (backups such as
/// `*.preexisting`, a build directory, a stray file) is left out, so it never
/// reaches OBS. Dot-entries such as `.osc` are not part of the package and are
/// not listed.
pub fn publish_plan(checkout_dir: &Path) -> PublishPlan {
    let mut names: Vec<String> = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();

    let spec_text: String = names
        .iter()
        .filter(|n| n.ends_with(".spec"))
        .filter_map(|n| fs::read_to_string(checkout_dir.join(n)).ok())
        .collect();
    let sources = spec_source_names(&spec_text);
    let series: Vec<String> = fs::read_to_string(checkout_dir.join("series"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let mut plan = PublishPlan::default();
    for name in names {
        if is_publishable(&name, &spec_text, &series, &sources) {
            plan.files.push(name);
        } else {
            plan.skipped.push(name);
        }
    }
    plan
}

/// The file names the spec's `Source:`/`Patch:` lines stand for: a URL's last
/// segment, or the name a `#/name` fragment gives it. A name that holds a macro
/// keeps it, as a pattern [`matches_source_name`] reads.
fn spec_source_names(spec_text: &str) -> Vec<String> {
    // `Version`/`Name` are known, so a release archive of another version is
    // not named by `foo-%{version}.tar.gz`; any other macro stays a wildcard.
    let header = |tag: &str| {
        spec_text.lines().find_map(|l| {
            let (t, v) = l.split_once(':')?;
            t.trim()
                .eq_ignore_ascii_case(tag)
                .then(|| v.trim().to_string())
        })
    };
    let (name, version) = (header("Name"), header("Version"));
    let expand = |value: &str| {
        let mut out = value.to_string();
        for (macros, known) in [
            (["%{version}", "%version"], &version),
            (["%{name}", "%name"], &name),
        ] {
            if let Some(known) = known.as_deref().filter(|k| !k.contains('%')) {
                for m in macros {
                    out = out.replace(m, known);
                }
            }
        }
        out
    };
    spec_text
        .lines()
        .filter_map(|line| {
            let (tag, value) = line.split_once(':')?;
            let tag = tag.trim().to_ascii_lowercase();
            let numbered = |prefix: &str| {
                tag.strip_prefix(prefix)
                    .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
            };
            if !(numbered("source") || numbered("patch")) {
                return None;
            }
            let value = expand(value.trim());
            let value = value.as_str();
            let name = match value.split_once("#/") {
                Some((_, renamed)) => renamed,
                None => value.rsplit('/').next().unwrap_or(value),
            };
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Whether `name` is the file a source `pattern` stands for. Each macro in the
/// pattern (`%{version}`, `%version`) matches any text.
fn matches_source_name(pattern: &str, name: &str) -> bool {
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = pattern;
    while let Some(at) = rest.find('%') {
        parts.push(&rest[..at]);
        let after = &rest[at + 1..];
        rest = if let Some(body) = after.strip_prefix('{') {
            body.split_once('}').map_or("", |(_, tail)| tail)
        } else {
            after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
        };
    }
    parts.push(rest);
    let (first, tail) = parts.split_first().expect("parts is never empty");
    let Some(mut remaining) = name.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = tail.split_last() else {
        return remaining.is_empty();
    };
    for part in middle {
        match remaining.find(part) {
            Some(at) => remaining = &remaining[at + part.len()..],
            None => return false,
        }
    }
    remaining.ends_with(last) && remaining.len() >= last.len()
}

/// Whether `name` is a build-recipe file (`_multibuild`, `_constraints`) that the
/// spec has no use for. `_multibuild` declares flavors, which only a spec that
/// reads `%flavor` builds differently. `_constraints` asks OBS for build
/// resources: a pure-Python (`BuildArch: noarch`) spec that does not mention
/// constraints or `%limit_build` compiles nothing that needs them, so one
/// inherited from a Factory recipe of another shape is a leftover. A spec that
/// builds native code keeps it: the agent writes it without touching the spec.
fn recipe_file_unused(name: &str, spec_text: &str) -> bool {
    match name {
        "_multibuild" => !["%{flavor}", "%flavor", "%{?flavor", "%{!?flavor"]
            .iter()
            .any(|m| spec_text.contains(m)),
        "_constraints" => {
            let noarch = spec_text.lines().any(|l| {
                let l = l.trim();
                l.get(..9)
                    .is_some_and(|p| p.eq_ignore_ascii_case("buildarch"))
                    && l.ends_with("noarch")
            });
            noarch && !spec_text.contains("constraints") && !spec_text.contains("%limit_build")
        }
        _ => false,
    }
}

/// Delete the build-recipe files of the checkout that its spec has no use for
/// ([`recipe_file_unused`]), so an upload never carries them and OBS drops the
/// tracked copy with the next commit. Returns the names removed.
pub fn drop_unused_recipe_files(checkout_dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return Vec::new();
    };
    let spec_text: String = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "spec"))
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    if spec_text.is_empty() {
        return Vec::new();
    }
    ["_constraints", "_multibuild"]
        .into_iter()
        .filter(|name| {
            checkout_dir.join(name).is_file()
                && recipe_file_unused(name, &spec_text)
                && fs::remove_file(checkout_dir.join(name)).is_ok()
        })
        .map(str::to_string)
        .collect()
}

fn is_publishable(name: &str, spec_text: &str, series: &[String], sources: &[String]) -> bool {
    // `_multibuild` declares flavors that only a spec which reads `%flavor` builds
    // differently. One left behind when a source recipe is swapped for a wheel
    // repackage (python-torch: `standard` and `openmpi4`) makes OBS build the same
    // spec once per flavor, so it goes, and is removed from OBS with the commit.
    if recipe_file_unused(name, spec_text) {
        return false;
    }
    // A wheel repackage compiles nothing and fetches no source tree: the
    // source recipe's `_service` (tar_scm), `_constraints` (build resources) and
    // `.obsinfo` are leftovers of the recipe the wheel switch replaced.
    let wheel_spec = sources.iter().any(|s| s.ends_with(".whl"));
    if wheel_spec
        && (["_service", "_servicedata", "_constraints"].contains(&name)
            || name.ends_with(".obsinfo"))
    {
        return false;
    }
    // An archive no `Source:` line names (the previous release's tarball or
    // vendor archive, the 784 MB source tarball of a switched recipe) is not
    // needed by `%prep`. Only judged when the spec names sources at all.
    const ARCHIVES: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar", ".tgz", ".tbz", ".txz", ".zip",
        ".crate", ".whl",
    ];
    if !sources.is_empty()
        && ARCHIVES.iter().any(|s| name.ends_with(s))
        && !sources.iter().any(|p| matches_source_name(p, name))
    {
        return false;
    }
    const FIXED: &[&str] = &[
        "_service",
        "_servicedata",
        "_multibuild",
        "_constraints",
        "_link",
        "series",
        // The audit trail of a Cargo workspace repair (see the pipeline).
        "cargo-workspace-fix.txt",
    ];
    const SUFFIXES: &[&str] = &[
        ".spec",
        ".changes",
        ".rpmlintrc",
        ".obsinfo",
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tar.zst",
        ".tar",
        ".tgz",
        ".tbz",
        ".txz",
        ".zip",
        ".crate",
        // A prebuilt wheel used as the source, and the pristine copy of an
        // archive the pipeline rewrote (kept so its provenance is auditable).
        ".whl",
        ".orig",
        // Detached signatures and keyrings that ship beside a source archive.
        ".sig",
        ".asc",
        ".keyring",
    ];
    if FIXED.contains(&name)
        || SUFFIXES.iter().any(|s| name.ends_with(s))
        || sources.iter().any(|p| matches_source_name(p, name))
    {
        return true;
    }
    let is_patch = name.ends_with(".patch") || name.ends_with(".diff");
    is_patch && (series.iter().any(|s| s == name) || spec_text.contains(name))
}

/// Delete zero-byte `*.rpmlintrc` files and the `SourceN:` lines of the spec
/// that name them: an empty filter file is noise in the package. Returns the
/// names removed.
pub fn drop_empty_rpmlintrc(checkout_dir: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(rd) = fs::read_dir(checkout_dir) else {
        return removed;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in &entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(".rpmlintrc")
            && e.metadata().is_ok_and(|m| m.is_file() && m.len() == 0)
            && fs::remove_file(e.path()).is_ok()
        {
            removed.push(name);
        }
    }
    if removed.is_empty() {
        return removed;
    }
    for e in entries {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "spec") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let kept: String = text
            .split_inclusive('\n')
            .filter(|l| {
                let Some((tag, value)) = l.split_once(':') else {
                    return true;
                };
                let tag = tag.trim().to_ascii_lowercase();
                let is_source = tag
                    .strip_prefix("source")
                    .is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()));
                !(is_source && value.contains("rpmlintrc"))
            })
            .collect();
        if kept != text {
            let _ = fs::write(&path, kept);
        }
    }
    removed
}

/// Names OBS already tracks for this checkout (`.osc/_files`).
fn tracked_files(checkout_dir: &Path) -> Vec<String> {
    let xml = fs::read_to_string(checkout_dir.join(".osc").join("_files")).unwrap_or_default();
    xml.split("<entry ")
        .skip(1)
        .filter_map(|chunk| {
            extract_xml_attr(&chunk[..chunk.find('>').unwrap_or(chunk.len())], "name")
        })
        .collect()
}

/// Bring back what the spec names as a `Source:`/`Patch:` and OBS tracks but the
/// checkout lost (an earlier run's `osc rm`, a file the agent deleted): without
/// it `%prep` fails, and the commit would carry the deletion. Returns the names
/// restored with `osc revert`.
pub fn restore_named_sources(checkout_dir: &Path, emit: Emit<'_>) -> Vec<String> {
    let spec_text: String = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".spec"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .collect();
    // Only a name that is exact: a wildcard would also bring back the previous
    // release's archive, which a version update removes on purpose.
    let sources: Vec<String> = spec_source_names(&spec_text)
        .into_iter()
        .filter(|n| !n.contains('%'))
        .collect();
    let mut restored = Vec::new();
    for name in tracked_files(checkout_dir) {
        if checkout_dir.join(&name).exists()
            || !sources.iter().any(|p| matches_source_name(p, &name))
        {
            continue;
        }
        match proc::capture("osc", &["revert", name.as_str()], checkout_dir, emit) {
            Ok(out) if out.success() => restored.push(name),
            Ok(out) => ap_core::diag!("  osc revert {name}: {}", first_line(&out.stderr)),
            Err(e) => ap_core::diag!("  osc revert {name}: {e}"),
        }
    }
    restored
}

/// Files OBS tracks that [`publish_plan`] says are not part of the package (an
/// archive no `Source:` names any more, a dropped rpmlintrc, a leftover),
/// scheduled for removal with `osc rm`. They stay on OBS otherwise, as the
/// 42 MB sdist a wheel switch replaced. Returns the names removed, which the
/// commit must list.
pub fn remove_tracked_strays(
    checkout_dir: &Path,
    skipped: &[String],
    emit: Emit<'_>,
) -> Vec<String> {
    let tracked = tracked_files(checkout_dir);
    let mut removed = Vec::new();
    for name in skipped {
        if !tracked.contains(name) {
            continue;
        }
        match proc::capture("osc", &["rm", "--force", name.as_str()], checkout_dir, emit) {
            Ok(out) if out.success() => removed.push(name.clone()),
            Ok(out) => ap_core::diag!("  osc rm {name}: {}", first_line(&out.stderr)),
            Err(e) => ap_core::diag!("  osc rm {name}: {e}"),
        }
    }
    // A tracked file that is gone from the checkout was deleted on purpose (the
    // old release's tarball when a package moves to a new version): the commit
    // must carry the deletion, or OBS keeps both releases' archives. `osc
    // addremove <name>` marks it, `osc ci <name>` commits it.
    for name in &tracked {
        if !checkout_dir.join(name).exists() && !removed.contains(name) && !skipped.contains(name) {
            removed.push(name.clone());
        }
    }
    removed
}

/// Commit the package to OBS, triggering a remote build.
///
/// Only `files` are staged and committed. `osc addremove` and `osc ci` both
/// take file arguments, so a file outside the list is neither tracked nor
/// pushed, even when it is modified.
pub fn commit(
    checkout_dir: &Path,
    message: &str,
    files: &[String],
    emit: Emit<'_>,
) -> Result<(), Error> {
    if files.is_empty() {
        return Err(Error::NothingToPublish(checkout_dir.to_path_buf()));
    }

    // A `write_atomic` temp file left by a killed run must not linger in the
    // checkout where a later add could pick it up.
    ap_core::fsx::sweep_stale_temp_files(checkout_dir, std::time::Duration::from_secs(24 * 3600));

    // Track new and deleted files among the listed ones. A failure is not
    // fatal (a new package may have nothing to add), but `ci` then commits
    // without them, so say so.
    let mut add = vec!["addremove".to_string()];
    add.extend(files.iter().cloned());
    match proc::capture("osc", &add, checkout_dir, emit) {
        Ok(out) if !out.success() => ap_core::diag!(
            "  osc addremove in {}: exit {}: {}",
            checkout_dir.display(),
            out.code,
            first_line(&out.stderr)
        ),
        Err(e) => ap_core::diag!("  osc addremove in {}: {e}", checkout_dir.display()),
        Ok(_) => {}
    }

    let mut ci = vec!["ci".to_string(), "-m".to_string(), message.to_string()];
    ci.extend(files.iter().cloned());
    let out = proc::capture_with_timeout("osc", &ci, checkout_dir, emit, OSC_TRANSFER_TIMEOUT)
        .map_err(during(format!("ci in {}", checkout_dir.display())))?;

    if !out.success() {
        // "nothing to do" means files are already committed — not an error.
        if out.stdout.contains("nothing to do") || out.stderr.contains("nothing to do") {
            return Ok(());
        }
        return Err(Error::OscFailed {
            cmd: format!("ci -m {message:?} in {}", checkout_dir.display()),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Query the OBS build status for one repo/arch combination.
///
/// Reads `/build/<project>/_result` (XML) and interprets it with
/// [`obs_status::resolve`]: a bare `finished` is resolved through its
/// `<details>` verdict, and a terminal result in a `dirty` repository is only
/// trusted once the job history confirms it was built from the source
/// revision currently on the server (otherwise `Unknown("stale: ...")`).
///
/// If the API call cannot run or its answer lacks the package, falls back to
/// scraping `osc results` so a surprising response shape degrades to the old
/// behaviour instead of breaking the build loop. An HTTP error from the API
/// (401, 429, 503, ...) is returned as is: the fallback would meet the same
/// refusal and only add load.
pub fn get_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    if let Some(status) = get_build_status_xml(project, pkg_name, repo, arch, false, None)? {
        return Ok(status);
    }
    get_build_status_from_results_table(project, pkg_name, repo, arch, emit)
}

/// [`get_build_status`] for a package whose sources were just committed: a
/// built verdict only counts once the job history shows it came from the
/// revision now on the server (see [`obs_status::Evidence::confirm`]), so the
/// previous revision's `failed`/`succeeded` is never mistaken for the result.
/// Without the XML answer there is no job history to check, so the table
/// fallback applies as usual. `committed` is when the commit was done: the
/// `_result` document is never older than that (see [`ResultsCache::get`]).
fn get_build_status_after_commit(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    committed: Instant,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    if let Some(status) =
        get_build_status_xml(project, pkg_name, repo, arch, true, Some(committed))?
    {
        return Ok(status);
    }
    get_build_status_from_results_table(project, pkg_name, repo, arch, emit)
}

/// What every repository/arch of `pkg_name` says, as one status: `Succeeded`
/// only when each result of the package (all repositories and architectures,
/// multibuild flavors included) is succeeded, excluded or disabled; otherwise
/// the first result that is not, with the `repo/arch` it is from. For
/// deciding whether the package may be submitted, where a build that is green
/// on `repo`/`arch` but failed on another architecture is not a good update.
/// `repo`/`arch` is read the careful way ([`get_build_status`]); the others
/// by their plain verdict, polled for a bounded time while any is still
/// scheduled, blocked or building.
pub fn get_overall_build_status(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<(BuildStatus, Option<String>), Error> {
    const MAX_WAIT: Duration = Duration::from_secs(1800);
    let own = get_build_status(project, pkg_name, repo, arch, emit)?;
    if !own.is_success() {
        return Ok((own, Some(format!("{repo}/{arch}"))));
    }
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    let path = format!(
        "/build/{}/_result?package={}&multibuild=1",
        enc(project),
        enc(pkg_name)
    );
    let start = Instant::now();
    let mut unchanged: u32 = 0;
    let mut last: Option<(BuildStatus, Option<String>)> = None;
    loop {
        let xml = osc_api_checked(&path).map_err(|e| match e {
            ApiFailure::Http(code, msg) => Error::OscFailed {
                cmd: format!("api {path}"),
                code: i32::from(code),
                stderr: msg,
            },
            ApiFailure::Other(msg) => Error::NotActionable(msg),
        })?;
        let entries =
            obs_status::try_parse_results(&xml).map_err(|e| Error::NotActionable(e.to_string()))?;
        let overall = overall_status(&entries, pkg_name);
        let active = matches!(
            overall.0,
            BuildStatus::Scheduled | BuildStatus::Blocked | BuildStatus::Building
        ) || overall.0 == BuildStatus::Unknown("finished".to_string());
        if !active || start.elapsed() >= MAX_WAIT || crate::proc::shutting_down() {
            return Ok(overall);
        }
        unchanged = if last.as_ref() == Some(&overall) {
            unchanged.saturating_add(1)
        } else {
            0
        };
        last = Some(overall);
        sleep_interruptible(poll_delay(unchanged, 0, jitter()));
    }
}

/// The aggregate of [`get_overall_build_status`], from a `_result` document's
/// entries: only the package and its `<package>:<flavor>` multibuild entries
/// count. A result that failed outranks one still going, since waiting on the
/// latter cannot change the outcome. Excluded and disabled results are out of
/// scope; the `repo/arch` of the blocking result comes with the status.
fn overall_status(
    entries: &[obs_status::ResultEntry],
    pkg_name: &str,
) -> (BuildStatus, Option<String>) {
    let flavor_prefix = format!("{pkg_name}:");
    let statuses: Vec<(BuildStatus, String)> = entries
        .iter()
        .filter(|e| e.pkg == pkg_name || e.pkg.starts_with(&flavor_prefix))
        .map(|e| {
            (
                obs_status::resolve_lenient(e),
                format!("{}/{}", e.repo, e.arch),
            )
        })
        .collect();
    let blocks = |s: &BuildStatus| {
        !matches!(
            s,
            BuildStatus::Succeeded | BuildStatus::Excluded | BuildStatus::Disabled
        )
    };
    if let Some((bad, at)) = statuses
        .iter()
        .find(|(s, _)| blocks(s) && s.is_terminal())
        .or_else(|| statuses.iter().find(|(s, _)| blocks(s)))
    {
        return (bad.clone(), Some(at.clone()));
    }
    if statuses.iter().any(|(s, _)| s.is_success()) {
        (BuildStatus::Succeeded, None)
    } else {
        (BuildStatus::Unknown("no result built".to_string()), None)
    }
}

/// The `osc results` human-table path only — no XML, no staleness
/// resolution via job history. Used by [`get_build_status`] as its fallback
/// when the XML `_result` document is unreadable, and by [`obs_build`] as an
/// independent second opinion when the XML path's answer has stopped
/// changing for a long time: the two paths hit different OBS code (an XML
/// API document vs. a human-table-rendering endpoint), so if one of them is
/// silently serving a stale/cached snapshot, the other is the cheapest way
/// to notice without guessing at why.
fn get_build_status_from_results_table(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<BuildStatus, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let cmd = format!("results --verbose -r {repo} -a {arch} {project} {pkg_name}");
    // `--verbose` is what carries a `finished` result's verdict.
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "results",
            "--verbose",
            "-r",
            repo,
            "-a",
            arch,
            project,
            pkg_name,
        ],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(parse_single_build_status(&out.stdout, pkg_name, repo, arch))
}

/// Per-call limit for the short, network-bound `osc` queries used while
/// polling. Generous for a healthy OBS, but finite: see
/// [`proc::capture_with_timeout`].
const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound for `osc co`, `up` and `ci`, which move a package's sources.
const OSC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Bound for `osc buildlog` and `getbinaries`, which move whole logs and RPM
/// sets and so run longer than a source transfer.
const OSC_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Run a short network-bound `osc` query in the current directory. A launch
/// failure or timeout is reported and yields `None`, for callers that treat a
/// failed query as "no answer" rather than as an error.
fn osc_query(args: &[&str], emit: Emit<'_>, what: &str) -> Option<Captured> {
    proc::capture_with_timeout("osc", args, &anywhere(), emit, OSC_QUERY_TIMEOUT)
        .map_err(|e| ap_core::diag!("  osc {what}: {e}"))
        .ok()
}

/// Why an `osc api` read gave no document.
#[derive(Clone)]
enum ApiFailure {
    /// The server answered with this HTTP status (401, 429, 503, ...): asking
    /// again through another `osc` command will not get a different answer.
    Http(u16, String),
    /// `osc` could not run, timed out, or failed without an HTTP status.
    Other(String),
}

/// The HTTP status in an `osc` error message, in either wording `osc`
/// versions use (`HTTP Error 503: ...`, `503 Server Error: ...`).
fn http_status(stderr: &str) -> Option<u16> {
    let digits = |s: &str| {
        let n: String = s.chars().take_while(char::is_ascii_digit).collect();
        (n.len() == 3).then(|| n.parse().ok()).flatten()
    };
    if let Some((_, rest)) = stderr.split_once("HTTP Error ") {
        return digits(rest);
    }
    stderr
        .split_whitespace()
        .zip(stderr.split_whitespace().skip(1))
        .find_map(|(code, kind)| {
            matches!(kind, "Client" | "Server")
                .then(|| digits(code))
                .flatten()
        })
}

/// `osc api <path>` stdout, or why there is none.
fn osc_api_checked(path: &str) -> Result<String, ApiFailure> {
    let out = proc::capture_with_timeout(
        "osc",
        &["api", path],
        &anywhere(),
        Emit::quiet(),
        OSC_QUERY_TIMEOUT,
    )
    .map_err(|e| ApiFailure::Other(format!("osc api {path}: {e}")))?;
    if !out.success() {
        let msg = format!("osc api {path}: exit {}: {}", out.code, out.stderr.trim());
        return Err(match http_status(&out.stderr) {
            Some(code) => ApiFailure::Http(code, msg),
            None => ApiFailure::Other(msg),
        });
    }
    Ok(out.stdout)
}

/// The end of a package's latest build log for `repo`/`arch`, where the
/// rpmlint block is, or `None` when OBS has no log for it.
pub fn fetch_build_log_tail(project: &str, pkg: &str, repo: &str, arch: &str) -> Option<String> {
    const KEEP: usize = 1 << 20;
    check_obs_name("project", project).ok()?;
    check_obs_name("package", pkg).ok()?;
    let log = osc_api(&format!(
        "/build/{}/{}/{}/{}/_log?nostream=1&start=0",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg)
    ))?;
    if log.len() <= KEEP {
        return Some(log);
    }
    let mut from = log.len() - KEEP;
    while !log.is_char_boundary(from) {
        from += 1;
    }
    Some(log[from..].to_string())
}

/// `osc api <path>` stdout, or `None` on any failure or timeout.
fn osc_api(path: &str) -> Option<String> {
    osc_api_checked(path)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()
}

pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encode one URL component. Package names like `libsigc++` contain
/// `+`, which a query string would otherwise read as a space, and `&` or `#`
/// would end the value outright.
fn enc(s: &str) -> std::borrow::Cow<'_, str> {
    urlencoding::encode(s)
}

/// How long a project-wide `_result` document is shared between waiters.
const RESULTS_TTL: Duration = Duration::from_secs(5);

/// How long a failed fetch is served to the waiters that ask right after it,
/// so a refusing server is asked once, not once per waiter.
const RESULTS_ERROR_TTL: Duration = Duration::from_secs(1);

/// The longest a waiter sits behind another waiter's fetch before fetching
/// itself (a little over [`OSC_QUERY_TIMEOUT`]).
const RESULTS_FLIGHT_WAIT: Duration = Duration::from_secs(70);

type ResultsKey = (String, String, String);

enum Slot {
    /// A fetch is running; `id` tells it apart from a later takeover.
    Fetching { id: u64 },
    /// The outcome of a fetch that began at `started` and ended at `at`.
    Ready {
        started: Instant,
        at: Instant,
        result: Result<Arc<str>, ApiFailure>,
    },
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<ResultsKey, Slot>,
    next_flight: u64,
    /// Bumped per project by [`ResultsCache::invalidate`]: a fetch that began
    /// under an older epoch is not stored.
    epochs: HashMap<String, u64>,
}

/// The project-wide `_result` documents, one per (project, repository, arch),
/// shared by every waiter of the process. Every OBS call is an `osc`
/// subprocess, so tens of waiters polling their own package would start tens
/// of Python interpreters per interval; here they trigger one fetch per
/// [`RESULTS_TTL`] and read their package out of the same document.
struct ResultsCache {
    state: Mutex<CacheState>,
    done: std::sync::Condvar,
}

/// Clears a flight's `Fetching` slot if the fetch unwinds, so the waiters
/// behind it neither hang nor wait for a result that will never come.
struct Flight<'a> {
    cache: &'a ResultsCache,
    key: &'a ResultsKey,
    id: u64,
    landed: bool,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if self.landed {
            return;
        }
        let mut st = self.cache.lock();
        if matches!(st.slots.get(self.key), Some(Slot::Fetching { id, .. }) if *id == self.id) {
            st.slots.remove(self.key);
        }
        drop(st);
        self.cache.done.notify_all();
    }
}

impl ResultsCache {
    fn new() -> Self {
        ResultsCache {
            state: Mutex::new(CacheState::default()),
            done: std::sync::Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The document for `key`, from `fetch` unless a shared one is at most
    /// `ttl` old. Waiters for a key whose fetch is running wait for it instead
    /// of starting another. `newer_than` demands a document whose fetch began
    /// at or after that instant (a waiter that has just committed cannot use
    /// what was asked before): an older one is not served, and an older fetch
    /// in flight is waited out and then replaced.
    fn get(
        &self,
        key: &ResultsKey,
        ttl: Duration,
        newer_than: Option<Instant>,
        fetch: impl FnOnce() -> Result<String, ApiFailure>,
    ) -> Result<Arc<str>, ApiFailure> {
        let deadline = Instant::now() + RESULTS_FLIGHT_WAIT;
        let mut st = self.lock();
        let (id, started, epoch) = loop {
            let fresh_enough = |started: &Instant| newer_than.is_none_or(|t| *started >= t);
            match st.slots.get(key) {
                Some(Slot::Ready {
                    started,
                    at,
                    result,
                }) if fresh_enough(started)
                    && at.elapsed()
                        < if result.is_ok() {
                            ttl
                        } else {
                            ttl.min(RESULTS_ERROR_TTL)
                        } =>
                {
                    return result.clone();
                }
                Some(Slot::Fetching { .. }) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !left.is_zero() {
                        st = self
                            .done
                            .wait_timeout(st, left)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                        continue;
                    }
                }
                _ => {}
            }
            let id = st.next_flight;
            st.next_flight += 1;
            let started = Instant::now();
            st.slots.insert(key.clone(), Slot::Fetching { id });
            break (id, started, st.epochs.get(&key.0).copied().unwrap_or(0));
        };
        drop(st);

        let mut flight = Flight {
            cache: self,
            key,
            id,
            landed: false,
        };
        let result = fetch().map(|doc| Arc::from(doc.as_str()));
        let mut st = self.lock();
        if matches!(st.slots.get(key), Some(Slot::Fetching { id: i, .. }) if *i == id) {
            if st.epochs.get(&key.0).copied().unwrap_or(0) == epoch {
                st.slots.insert(
                    key.clone(),
                    Slot::Ready {
                        started,
                        at: Instant::now(),
                        result: result.clone(),
                    },
                );
            } else {
                st.slots.remove(key);
            }
        }
        flight.landed = true;
        drop(st);
        self.done.notify_all();
        result
    }

    /// Forget `project`'s documents: its sources changed, so what was fetched
    /// so far describes the old state. A fetch in flight is not stored.
    fn invalidate(&self, project: &str) {
        let mut st = self.lock();
        *st.epochs.entry(project.to_string()).or_insert(0) += 1;
        st.slots
            .retain(|k, slot| k.0 != project || matches!(slot, Slot::Fetching { .. }));
    }
}

static RESULTS_CACHE: LazyLock<ResultsCache> = LazyLock::new(ResultsCache::new);

/// Every entry of the `_result` document for `pkg`'s repository/arch, when the
/// document has `pkg` in it. The project-wide document comes from `cache`;
/// one that is cut off, or lacks the package (not scheduled yet, or fetched
/// before it was added), is replaced by `fetch_package`, the document of the
/// package alone (with its multibuild flavors). A failure of the shared fetch
/// is returned as is: asking again another way meets the same refusal.
#[allow(clippy::too_many_arguments)]
fn package_entries(
    cache: &ResultsCache,
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
    fetch_project: impl FnOnce() -> Result<String, ApiFailure>,
    fetch_package: impl FnOnce() -> Result<String, ApiFailure>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    let key = (project.to_string(), repo.to_string(), arch.to_string());
    let doc = cache.get(&key, RESULTS_TTL, fresh_since, fetch_project)?;
    if let Ok(entries) = obs_status::try_parse_results(&doc)
        && obs_status::entry_in(&entries, repo, arch, pkg).is_some()
    {
        return Ok(Some(entries));
    }
    let xml = fetch_package()?;
    let entries = obs_status::parse_results(&xml);
    let found = obs_status::entry_in(&entries, repo, arch, pkg).is_some();
    Ok(found.then_some(entries))
}

/// [`package_entries`] against OBS. The names must be valid OBS names.
fn obs_package_entries(
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    // `multibuild=1` adds the package's `<pkg>:<flavor>` entries to the
    // single-package document; the project-wide one lists them anyway.
    let (project_path, package_path) = (
        format!(
            "/build/{project}/_result?repository={}&arch={}",
            enc(repo),
            enc(arch)
        ),
        format!(
            "/build/{project}/_result?package={}&repository={}&arch={}&multibuild=1",
            enc(pkg),
            enc(repo),
            enc(arch)
        ),
    );
    package_entries(
        &RESULTS_CACHE,
        project,
        repo,
        arch,
        pkg,
        fresh_since,
        || osc_api_checked(&project_path),
        || osc_api_checked(&package_path),
    )
}

/// `fresh_since`: only a document fetched after that instant may be used
/// (see [`ResultsCache::get`]); `None` accepts one a few seconds old.
fn get_build_status_xml(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    confirm: bool,
    fresh_since: Option<Instant>,
) -> Result<Option<BuildStatus>, Error> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Ok(None);
    }
    let entries = match obs_package_entries(project, repo, arch, pkg_name, fresh_since) {
        Ok(Some(entries)) => entries,
        Ok(None) => return Ok(None),
        // An HTTP error (login, rate limit, outage) would repeat through the
        // `osc results` fallback, which only adds another request to a
        // server that is already refusing: report it.
        Err(ApiFailure::Http(code, msg)) => {
            return Err(Error::OscFailed {
                cmd: format!("api /build/{project}/_result"),
                code: i32::from(code),
                stderr: msg,
            });
        }
        Err(ApiFailure::Other(msg)) => {
            ap_core::diag!("  {msg}");
            return Ok(None);
        }
    };
    let Some(entry) = obs_status::entry_in(&entries, repo, arch, pkg_name) else {
        return Ok(None);
    };
    // The source revision is the package's; every flavor builds from it.
    let mut source: Option<Option<obs_status::SourceInfo>> = None;
    let mut resolve_entry = |entry: &obs_status::ResultEntry| {
        let evidence = if obs_status::needs_evidence(entry, confirm) {
            obs_status::Evidence {
                confirm,
                source: source
                    .get_or_insert_with(|| {
                        osc_api(&format!("/source/{project}/{}?view=info", enc(pkg_name)))
                            .and_then(|x| obs_status::parse_source_info(&x))
                    })
                    .clone(),
                job: osc_api(&format!(
                    "/build/{project}/{}/{}/_jobhistory?package={}&limit=1",
                    enc(repo),
                    enc(arch),
                    enc(&entry.pkg)
                ))
                .and_then(|x| obs_status::parse_latest_job(&x)),
            }
        } else {
            obs_status::Evidence {
                confirm,
                ..Default::default()
            }
        };
        obs_status::resolve(entry, &evidence)
    };
    let base = resolve_entry(entry);
    let flavors: Vec<BuildStatus> = obs_status::flavors_in(&entries, repo, arch, pkg_name)
        .iter()
        .map(&mut resolve_entry)
        .collect();
    Ok(Some(obs_status::combine(base, &flavors)))
}

#[cfg(test)]
mod results_cache_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const LONG: Duration = Duration::from_secs(60);

    fn key(project: &str) -> ResultsKey {
        (
            project.to_string(),
            "repo".to_string(),
            "x86_64".to_string(),
        )
    }

    fn doc(body: &str) -> Result<String, ApiFailure> {
        Ok(body.to_string())
    }

    #[test]
    fn concurrent_waiters_trigger_one_fetch() {
        let cache = ResultsCache::new();
        let fetches = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        cache.get(&key("p"), LONG, None, || {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(150));
                            doc("<resultlist/>")
                        })
                    })
                })
                .collect();
            for h in handles {
                assert_eq!(&*h.join().unwrap().ok().unwrap(), "<resultlist/>");
            }
        });
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_document_is_shared_until_it_expires() {
        let cache = ResultsCache::new();
        let fetches = AtomicUsize::new(0);
        let fetch = || {
            fetches.fetch_add(1, Ordering::SeqCst);
            doc("d")
        };
        for _ in 0..3 {
            cache.get(&key("p"), LONG, None, fetch).ok().unwrap();
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        // With no lifetime every read is a new fetch.
        cache
            .get(&key("p"), Duration::ZERO, None, fetch)
            .ok()
            .unwrap();
        cache
            .get(&key("p"), Duration::ZERO, None, fetch)
            .ok()
            .unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn keys_do_not_share_documents() {
        let cache = ResultsCache::new();
        let a = cache.get(&key("a"), LONG, None, || doc("a")).ok().unwrap();
        let b = cache.get(&key("b"), LONG, None, || doc("b")).ok().unwrap();
        let mut other_arch = key("a");
        other_arch.2 = "aarch64".to_string();
        let c = cache
            .get(&other_arch, LONG, None, || doc("c"))
            .ok()
            .unwrap();
        assert_eq!((&*a, &*b, &*c), ("a", "b", "c"));
    }

    #[test]
    fn a_fresh_read_skips_a_document_fetched_before_it() {
        let cache = ResultsCache::new();
        cache
            .get(&key("p"), LONG, None, || doc("old"))
            .ok()
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let committed = Instant::now();
        let fresh = cache
            .get(&key("p"), LONG, Some(committed), || doc("new"))
            .ok()
            .unwrap();
        assert_eq!(&*fresh, "new");
        // That document is itself fresh enough for the next such waiter.
        let again = cache
            .get(&key("p"), LONG, Some(committed), || doc("never"))
            .ok()
            .unwrap();
        assert_eq!(&*again, "new");
    }

    #[test]
    fn a_fresh_read_waits_out_an_older_fetch_and_then_fetches_itself() {
        let cache = ResultsCache::new();
        let fetches = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let slow = scope.spawn(|| {
                cache.get(&key("p"), LONG, None, || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    doc("before the commit")
                })
            });
            std::thread::sleep(Duration::from_millis(50));
            let committed = Instant::now();
            let fresh = cache
                .get(&key("p"), LONG, Some(committed), || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    doc("after the commit")
                })
                .ok()
                .unwrap();
            assert_eq!(&*fresh, "after the commit");
            assert_eq!(&*slow.join().unwrap().ok().unwrap(), "before the commit");
        });
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalidation_drops_the_documents_of_one_project() {
        let cache = ResultsCache::new();
        cache.get(&key("p"), LONG, None, || doc("p1")).ok().unwrap();
        cache.get(&key("q"), LONG, None, || doc("q1")).ok().unwrap();
        cache.invalidate("p");
        let p = cache.get(&key("p"), LONG, None, || doc("p2")).ok().unwrap();
        let q = cache.get(&key("q"), LONG, None, || doc("q2")).ok().unwrap();
        assert_eq!((&*p, &*q), ("p2", "q1"));
    }

    #[test]
    fn a_fetch_in_flight_during_an_invalidation_is_not_stored() {
        let cache = ResultsCache::new();
        let first = cache
            .get(&key("p"), LONG, None, || {
                cache.invalidate("p");
                doc("pre-commit")
            })
            .ok()
            .unwrap();
        // The fetcher still gets what it fetched, but nobody else does.
        assert_eq!(&*first, "pre-commit");
        let next = cache
            .get(&key("p"), LONG, None, || doc("post-commit"))
            .ok()
            .unwrap();
        assert_eq!(&*next, "post-commit");
    }

    #[test]
    fn a_failure_is_shared_briefly_and_not_kept() {
        let cache = ResultsCache::new();
        let fetches = AtomicUsize::new(0);
        let failing = || {
            fetches.fetch_add(1, Ordering::SeqCst);
            Err(ApiFailure::Http(503, "down".to_string()))
        };
        for _ in 0..3 {
            assert!(matches!(
                cache.get(&key("p"), LONG, None, failing),
                Err(ApiFailure::Http(503, _))
            ));
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        // Once the (short) error lifetime is over, it is asked again.
        let again = cache
            .get(&key("p"), Duration::ZERO, None, || doc("up"))
            .ok()
            .unwrap();
        assert_eq!(&*again, "up");
    }

    #[test]
    fn a_fetch_that_panics_does_not_block_the_next_one() {
        let cache = ResultsCache::new();
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.get(&key("p"), LONG, None, || panic!("osc exploded"));
        }));
        assert!(crashed.is_err());
        let next = cache
            .get(&key("p"), LONG, None, || doc("fine"))
            .ok()
            .unwrap();
        assert_eq!(&*next, "fine");
    }

    const PROJECT_DOC: &str = r#"<resultlist>
  <result project="p" repository="repo" arch="x86_64" dirty="true">
    <status package="foo" code="building"/>
    <status package="foo:mini" code="succeeded"/>
    <status package="bar" code="blocked"><details>waiting for foo</details></status>
  </result>
</resultlist>"#;

    fn entries_of(
        cache: &ResultsCache,
        pkg: &str,
        project: &AtomicUsize,
        single: &AtomicUsize,
        project_doc: &'static str,
        package_doc: &'static str,
    ) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
        package_entries(
            cache,
            "p",
            "repo",
            "x86_64",
            pkg,
            None,
            || {
                project.fetch_add(1, Ordering::SeqCst);
                doc(project_doc)
            },
            || {
                single.fetch_add(1, Ordering::SeqCst);
                doc(package_doc)
            },
        )
    }

    #[test]
    fn every_package_reads_its_entries_from_the_one_project_document() {
        let cache = ResultsCache::new();
        let (project, single) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let foo = entries_of(&cache, "foo", &project, &single, PROJECT_DOC, "")
            .ok()
            .unwrap()
            .unwrap();
        let bar = entries_of(&cache, "bar", &project, &single, PROJECT_DOC, "")
            .ok()
            .unwrap()
            .unwrap();
        assert_eq!(project.load(Ordering::SeqCst), 1);
        assert_eq!(single.load(Ordering::SeqCst), 0);
        // Multibuild flavors come with the package, as with a per-package read.
        let flavors = obs_status::flavors_in(&foo, "repo", "x86_64", "foo");
        assert_eq!(flavors.len(), 1);
        assert_eq!(flavors[0].pkg, "foo:mini");
        let entry = obs_status::entry_in(&bar, "repo", "x86_64", "bar").unwrap();
        assert_eq!(entry.details.as_deref(), Some("waiting for foo"));
        assert!(entry.dirty);
    }

    #[test]
    fn a_package_missing_from_the_shared_document_is_read_on_its_own() {
        let cache = ResultsCache::new();
        let (project, single) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let own = r#"<resultlist><result project="p" repository="repo" arch="x86_64">
            <status package="new" code="scheduled"/></result></resultlist>"#;
        let entries = entries_of(&cache, "new", &project, &single, PROJECT_DOC, own)
            .ok()
            .unwrap()
            .unwrap();
        assert_eq!(single.load(Ordering::SeqCst), 1);
        assert_eq!(entries[0].code, "scheduled");
        // Missing there too: no entry, the caller falls back to `osc results`.
        let none = entries_of(
            &cache,
            "ghost",
            &project,
            &single,
            PROJECT_DOC,
            "<resultlist/>",
        );
        assert!(none.ok().unwrap().is_none());
    }

    #[test]
    fn a_cut_off_shared_document_is_not_trusted() {
        let cache = ResultsCache::new();
        let (project, single) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let cut = &PROJECT_DOC[..PROJECT_DOC.len() - 30];
        let own = r#"<resultlist><result project="p" repository="repo" arch="x86_64">
            <status package="bar" code="failed"/></result></resultlist>"#;
        let entries = entries_of(&cache, "bar", &project, &single, cut, own)
            .ok()
            .unwrap()
            .unwrap();
        assert_eq!(entries[0].code, "failed");
        assert_eq!(single.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refusal_of_the_shared_fetch_is_passed_on_without_another_request() {
        let cache = ResultsCache::new();
        let single = AtomicUsize::new(0);
        let result = package_entries(
            &cache,
            "p",
            "repo",
            "x86_64",
            "foo",
            None,
            || Err(ApiFailure::Http(429, "slow down".to_string())),
            || {
                single.fetch_add(1, Ordering::SeqCst);
                doc("")
            },
        );
        assert!(matches!(result, Err(ApiFailure::Http(429, _))));
        assert_eq!(single.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod wait_pacing_tests {
    use super::*;

    const SECS: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn a_stale_status_is_checked_after_thirty_seconds_then_every_minute() {
        assert!(!recheck_due(true, SECS(29), None));
        assert!(recheck_due(true, SECS(30), None));
        assert!(!recheck_due(true, SECS(200), Some(SECS(59))));
        assert!(recheck_due(true, SECS(200), Some(SECS(60))));
    }

    #[test]
    fn any_other_unchanged_status_is_checked_once_after_five_minutes() {
        assert!(!recheck_due(false, SECS(299), None));
        assert!(recheck_due(false, SECS(300), None));
        assert!(!recheck_due(false, SECS(900), Some(SECS(600))));
    }

    #[test]
    fn a_status_awaiting_the_new_revision_never_backs_off_past_thirty_seconds() {
        assert_eq!(wait_polls(40, true), 7);
        assert_eq!(poll_delay(wait_polls(40, true), 0, 0.5), SECS(30));
        assert_eq!(wait_polls(2, true), 2);
        assert_eq!(poll_delay(wait_polls(40, false), 0, 0.5), SECS(60));
    }
}

/// Parsing half of the `osc results` fallback in [`get_build_status`], split
/// out so it is testable without shelling out to `osc`.
///
/// `osc results --verbose -r <repo> -a <arch> <project> <pkg>` prints one line
/// per result as `<repo> <arch> <pkg> <status>` (osc's `get_results` format,
/// columns in that order). The status is the rest of the line:
/// - a plain code (`succeeded`, `failed`, `scheduled`, ...), optionally with
///   `: <details>` after it, and `(unpublished)` for a build that succeeded
///   but is not published yet;
/// - `finished: <verdict>`: OBS hides the verdict behind the bare `finished`
///   code, and `--verbose` prints it as the details;
/// - `outdated (was: ...)` when the repository is dirty, or a trailing `*` in
///   the non-verbose form: the result may be for an older source revision.
///
/// Multi-line `unresolvable:` details only add lines that do not start with
/// the repo, so reading the first matching line is enough.
fn parse_single_build_status(stdout: &str, pkg_name: &str, repo: &str, arch: &str) -> BuildStatus {
    for line in stdout.lines() {
        let mut rest = line.trim_start();
        let mut columns = [""; 3];
        for col in &mut columns {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            *col = &rest[..end];
            rest = rest[end..].trim_start();
        }
        if columns != [repo, arch, pkg_name] {
            continue;
        }
        return status_from_osc_text(rest.trim());
    }
    BuildStatus::Unknown("no results".to_string())
}

/// The status part of one `osc results` line (see [`parse_single_build_status`]).
fn status_from_osc_text(text: &str) -> BuildStatus {
    // OBS marks a result as possibly older than the sources on the server:
    // the dirty-repository form, which `--verbose` spells `outdated (was: ..)`
    // and the plain form spells with a trailing `*`. Never trust those as-is:
    // reporting the stripped word as plain `Succeeded` would make the
    // `AlreadyBuilt` fast path in pipeline.rs believe a since-superseded
    // result still verifies the current revision. `Unknown` makes that path
    // fall through and resync instead.
    if text.starts_with("outdated") {
        return BuildStatus::Unknown(format!("stale: {text}"));
    }
    let (head, details) = match text.split_once(':') {
        Some((head, details)) => (head.trim(), Some(details.trim())),
        None => (text, None),
    };
    let starred = head.ends_with('*');
    let word = head.trim_end_matches('*').trim_end_matches("(unpublished)");
    if starred {
        return BuildStatus::Unknown(format!("stale: {word}"));
    }
    if word == "finished" {
        // The verdict is the first word of the details, if osc printed it.
        return match details.and_then(|d| d.split_whitespace().next()) {
            Some(verdict) => BuildStatus::from(verdict),
            None => BuildStatus::Unknown("finished".to_string()),
        };
    }
    BuildStatus::from(word)
}

#[cfg(test)]
mod single_build_status_tests {
    use super::*;

    const REPO: &str = "openSUSE_Tumbleweed";
    const ARCH: &str = "x86_64";

    fn parse(stdout: &str) -> BuildStatus {
        parse_single_build_status(stdout, "python-foo", REPO, ARCH)
    }

    #[test]
    fn a_plain_status_is_read_from_the_last_column() {
        // Real `osc results -r .. -a .. prj pkg` line: repo, arch, package, status.
        let stdout = "openSUSE_Tumbleweed  x86_64  python-foo  succeeded\n";
        assert_eq!(parse(stdout), BuildStatus::Succeeded);
    }

    #[test]
    fn a_starred_status_is_unknown_not_its_unstarred_meaning() {
        // The real incident this guards against: committing a new revision
        // and immediately watching build status used to report `Succeeded`
        // from the *previous* revision's still-displayed, now-stale result,
        // before OBS had even started rebuilding the new one.
        let stdout = "openSUSE_Tumbleweed  x86_64  python-foo  succeeded*\n";
        assert_eq!(
            parse(stdout),
            BuildStatus::Unknown("stale: succeeded".to_string())
        );
    }

    #[test]
    fn a_verbose_outdated_result_is_stale() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo outdated (was: finished: succeeded)\n";
        assert!(matches!(parse(stdout), BuildStatus::Unknown(s) if s.starts_with("stale: ")));
    }

    #[test]
    fn a_bare_finished_takes_its_verdict_from_the_details() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished: failed\n";
        assert_eq!(parse(stdout), BuildStatus::Failed);
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished: succeeded\n";
        assert_eq!(parse(stdout), BuildStatus::Succeeded);
    }

    #[test]
    fn a_finished_without_a_verdict_is_not_terminal() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo finished\n";
        let status = parse(stdout);
        assert_eq!(status, BuildStatus::Unknown("finished".to_string()));
        assert!(!status.is_terminal());
    }

    #[test]
    fn verbose_details_after_a_code_are_ignored() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo failed: some details\n";
        assert_eq!(parse(stdout), BuildStatus::Failed);
    }

    #[test]
    fn an_unpublished_success_counts_as_succeeded() {
        let stdout = "openSUSE_Tumbleweed x86_64 python-foo succeeded(unpublished)\n";
        assert_eq!(parse(stdout), BuildStatus::Succeeded);
    }

    #[test]
    fn lines_for_other_repos_arches_and_packages_are_skipped() {
        let stdout = "\
openSUSE_Leap x86_64 python-foo failed
openSUSE_Tumbleweed aarch64 python-foo failed
openSUSE_Tumbleweed x86_64 python-foo:flavor failed
openSUSE_Tumbleweed x86_64 python-foo building
";
        assert_eq!(parse(stdout), BuildStatus::Building);
        assert_eq!(
            parse_single_build_status("", "python-foo", REPO, ARCH),
            BuildStatus::Unknown("no results".to_string())
        );
    }
}

/// Query the OBS build status of every package in `project` for one
/// repo/arch in a single call.
///
/// Runs `osc results -r <repo> -a <arch> <project>` (no package name lists
/// every package). Unlike the single-package form, this layout is NOT
/// `<pkg> ... <status-word>` per line: `osc` prints the package names as an
/// indented tree, one per line, wrapping into as many same-order blocks as
/// fit the terminal width, and after each block a single status line packs
/// one letter code per package into fixed two-column slots (`<code><sep>`),
/// e.g. `. B U U . U ...  openSUSE_Tumbleweed x86_64 (published)`. A blank
/// (space) code means "disabled" — indistinguishable from the column
/// separator under naive whitespace splitting — hence the positional
/// indexing here instead of `split_whitespace`.
pub fn get_project_build_statuses(
    project: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<HashMap<String, BuildStatus>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    // XML first: one entry per package with a named code, instead of the
    // positional letter table below (which depends on terminal width and
    // `osc`'s padding). Fall back to the table if the API path yields nothing.
    if let Some(xml) = osc_api(&format!(
        "/build/{}/_result?repository={}&arch={}",
        enc(project),
        enc(repo),
        enc(arch)
    )) {
        let statuses = match project_statuses_from_xml(&xml, repo, arch) {
            Ok(statuses) => statuses,
            Err(e) => {
                // A cut-off listing must not read as a partial set of statuses.
                ap_core::diag!("  cannot read the OBS results listing: {e}");
                HashMap::new()
            }
        };
        if !statuses.is_empty() {
            return Ok(fold_flavor_statuses(statuses));
        }
    }

    // An empty map reads as "no package has a status", so a failed listing
    // is an error rather than an empty answer.
    let cmd = format!("results -r {repo} -a {arch} {project}");
    let out = proc::capture_with_timeout(
        "osc",
        &["results", "-r", repo, "-a", arch, project],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(cmd.clone()))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd,
            code: out.code,
            stderr: out.stderr,
        });
    }

    Ok(fold_flavor_statuses(parse_project_build_statuses(
        &out.stdout,
        repo,
        arch,
    )))
}

/// A package is only as built as its worst multibuild flavor: `python-mcp` is
/// `succeeded` while its `python-mcp:test` flavor is `unresolvable` (a test
/// dependency nothing provides), and counting the base alone as built left the
/// package out of the build queue with its missing dependency never reported.
/// A flavor that is not `succeeded`/`excluded`/`disabled` passes its status to
/// the base package when the base would otherwise count as built. The flavor
/// entries stay in the map.
fn fold_flavor_statuses(
    mut statuses: HashMap<String, BuildStatus>,
) -> HashMap<String, BuildStatus> {
    let blocks = |s: &BuildStatus| {
        !matches!(
            s,
            BuildStatus::Succeeded | BuildStatus::Excluded | BuildStatus::Disabled
        )
    };
    let flavors: Vec<(String, BuildStatus)> = statuses
        .iter()
        .filter(|(k, s)| k.contains(':') && blocks(s))
        // A `test` flavor whose tests FAIL builds nothing a dependent installs,
        // and the package it belongs to cannot be fixed by building it again:
        // python-redis (`python-redis:test` failed, `python-redis` succeeded) was
        // selected as unbuilt every round, finished "already built" every
        // round, and the requested root was never reached. Only a test flavor
        // that cannot even be set up (`unresolvable`, `broken`) still counts.
        .filter(|(k, s)| !(k.ends_with(":test") && matches!(s, BuildStatus::Failed)))
        .map(|(k, s)| (k.clone(), s.clone()))
        .collect();
    for (key, status) in flavors {
        let Some((base, _)) = key.split_once(':') else {
            continue;
        };
        if let Some(b) = statuses.get_mut(base)
            && b.is_success()
        {
            *b = status;
        }
    }
    statuses
}

/// Every package's status in ONE repository/architecture of a `_result`
/// document.
///
/// The document carries a `<result>` per repository/architecture the project
/// builds for, and the same package is routinely `succeeded` in one and
/// `excluded` or `unresolvable` in another (an `i586` repository next to
/// `x86_64`); only the pair asked for may contribute, or one package's state
/// somewhere else would stand in for the state of the build being made.
fn project_statuses_from_xml(
    xml: &str,
    repo: &str,
    arch: &str,
) -> Result<HashMap<String, BuildStatus>, String> {
    Ok(obs_status::try_parse_results(xml)?
        .into_iter()
        .filter(|e| e.repo == repo && e.arch == arch)
        .map(|e| (e.pkg.clone(), obs_status::resolve_lenient(&e)))
        .collect())
}

/// Parsing half of [`get_project_build_statuses`], split out so it is
/// testable without shelling out to `osc`.
fn parse_project_build_statuses(
    stdout: &str,
    repo: &str,
    arch: &str,
) -> HashMap<String, BuildStatus> {
    let marker = format!(" {repo} {arch} (");
    let mut statuses = HashMap::new();
    let mut pending_pkgs: Vec<String> = Vec::new();

    for line in stdout.lines() {
        if line.trim() == "Legend:" {
            break;
        }
        if let Some(marker_idx) = line.find(&marker) {
            let chars: Vec<char> = line.chars().collect();
            let n = pending_pkgs.len();
            // The code table is dense two-char slots (`<code><sep>`), but
            // real `osc` builds disagree on whether there's a leading sep
            // before the first code and a trailing one after the last: some
            // print it flush (`code sep code ... code`, length `2n-1`, codes
            // at `2*i`), others pad both ends (`sep code sep ... code sep`,
            // length `2n+1`, codes at `1 + 2*i`). Derive the offset from the
            // actual prefix length instead of hardcoding one — hardcoding
            // `2*i` silently read every code as its own separator space
            // against the padded form, making every package look
            // "disabled" and so falsely resolvable.
            let prefix_len = line[..marker_idx].chars().count() as isize;
            let offset = if n > 0 {
                ((prefix_len - 2 * n as isize + 1) / 2).max(0) as usize
            } else {
                0
            };
            for (i, pkg) in pending_pkgs.iter().enumerate() {
                if let Some(&c) = chars.get(offset + 2 * i)
                    && c != ' '
                {
                    statuses.insert(pkg.clone(), BuildStatus::from_code(c));
                }
            }
            pending_pkgs.clear();
            continue;
        }
        let name = line.trim_start_matches([' ', '|']).trim();
        if !name.is_empty() {
            pending_pkgs.push(name.to_string());
        }
    }

    statuses
}

/// Fetch the OBS build log into `log_path`.
///
/// Runs `osc buildlog <project> <package> <repo> <arch>` and writes
/// stdout to `log_path`. Returns `Ok` even when the exit code is non-zero
/// if some output was captured (partial logs are still useful).
pub fn fetch_buildlog(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["buildlog", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "buildlog {project} {pkg_name} {repo} {arch}"
    )))?;

    std::fs::write(log_path, &out.stdout)?;

    if !out.success() && out.stdout.is_empty() {
        return Err(Error::OscFailed {
            cmd: format!("buildlog {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Ask OBS to build `pkg_name` again from the sources it already has, and wait
/// (at most 90 s) until its status is no longer a final one. Without that wait
/// the build wait that follows would read the previous build's verdict, which
/// matches the current sources, as the answer. Returns whether the new build
/// was seen to start.
pub fn rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<bool, Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_QUERY_TIMEOUT,
    )
    .map_err(during(format!(
        "rebuild {project} {pkg_name} {repo} {arch}"
    )))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("rebuild {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) && !crate::proc::shutting_down() {
        if get_build_status(project, pkg_name, repo, arch, Emit::quiet())
            .is_ok_and(|s| !s.is_terminal())
        {
            return Ok(true);
        }
        sleep_interruptible(Duration::from_secs(5));
    }
    Ok(false)
}

/// Download built binaries from OBS into `dest_dir`.
///
/// How many of the binary RPMs OBS lists for `pkg_name` are now in `rpms_dir`.
///
/// Used after `osc getbinaries` to confirm it actually produced artifacts
/// rather than silently exiting 0 with an empty binary set (a misconfigured
/// publishing setup for the repo/arch pair). The RPMs are matched by the
/// names OBS lists, NOT by the source package name: a multi-flavor Python
/// package `python-events` ships `python313-Events-…rpm`, which no
/// `python-events-` prefix would ever match. If the listing cannot be read,
/// fall back to counting RPMs written in the last few minutes.
fn downloaded_rpm_count(listing: Option<&[String]>, rpms_dir: &std::path::Path) -> usize {
    match listing {
        Some(names) => names.iter().filter(|n| rpms_dir.join(n).is_file()).count(),
        None => count_recent_rpms(rpms_dir, Duration::from_secs(600)),
    }
}

/// The `.rpm` binaries OBS lists for `pkg_name` in `repo`/`arch`, or `None`
/// when the listing cannot be read.
fn binary_rpm_listing(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<Vec<String>> {
    osc_api(&format!(
        "/build/{project}/{}/{}/{}",
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpms(&xml))
}

/// The package name of an RPM file name: `python313-foo-1.0-2.1.noarch.rpm`
/// is `python313-foo`.
fn rpm_base_name(file: &str) -> Option<&str> {
    let (rest, _arch) = file.strip_suffix(".rpm")?.rsplit_once('.')?;
    let (rest, _release) = rest.rsplit_once('-')?;
    let (name, _version) = rest.rsplit_once('-')?;
    Some(name)
}

/// Delete the RPMs in `rpms_dir` that an earlier build of the same package
/// left behind: files named like one of `current` (same package name),
/// including a `current` file itself, so that a download which wrote nothing
/// is not mistaken for one that did by a stale file of the same name.
/// `rpms_dir` is shared by every package, so they would otherwise pile up and
/// satisfy a dependency with an old version.
fn remove_superseded_rpms(rpms_dir: &Path, current: &[String]) {
    let names: std::collections::HashSet<&str> =
        current.iter().filter_map(|f| rpm_base_name(f)).collect();
    let Ok(entries) = fs::read_dir(rpms_dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.ends_with(".src.rpm") {
            continue;
        }
        if rpm_base_name(&file).is_some_and(|n| names.contains(n))
            && let Err(e) = fs::remove_file(entry.path())
        {
            ap_core::diag!("  cannot remove the stale {file}: {e}");
        }
    }
}

/// The result a non-successful `status` of `pkg_name` comes from: the package
/// itself, or the `<pkg>:<flavor>` multibuild entry that failed while the
/// package built (`python-mcp:test`'s `%check`). The status of a multibuild
/// package is the worst of its entries, so the log that explains it is that
/// entry's, not the package's own, which can be a clean success.
fn failing_result(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    status: &BuildStatus,
) -> String {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return pkg_name.to_string();
    }
    osc_api(&format!(
        "/build/{project}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .and_then(|xml| failing_entry_name(&xml, repo, arch, pkg_name, status))
    .unwrap_or_else(|| pkg_name.to_string())
}

/// [`failing_result`] over a `_result` document: the package if its own entry
/// has `status`, else the first flavor entry that does.
fn failing_entry_name(
    xml: &str,
    repo: &str,
    arch: &str,
    pkg_name: &str,
    status: &BuildStatus,
) -> Option<String> {
    let base = obs_status::find_entry(xml, repo, arch, pkg_name);
    std::iter::once(base)
        .flatten()
        .chain(obs_status::flavor_entries(xml, repo, arch, pkg_name))
        .find(|e| obs_status::resolve_lenient(e) == *status)
        .map(|e| e.pkg)
        .filter(|name| is_valid_obs_name(name))
}

/// The `<pkg>:<flavor>` names of `pkg_name`'s multibuild flavors built for
/// `repo`/`arch`; empty for a plain package or when the listing cannot be read.
fn multibuild_flavors(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Vec::new();
    }
    osc_api(&format!(
        "/build/{project}/_result?package={}&repository={}&arch={}&multibuild=1",
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))
    .map(|xml| {
        obs_status::flavor_entries(&xml, repo, arch, pkg_name)
            .into_iter()
            .filter(|e| {
                is_valid_obs_name(&e.pkg)
                    && !matches!(
                        obs_status::resolve_lenient(e),
                        BuildStatus::Excluded | BuildStatus::Disabled
                    )
            })
            .map(|e| e.pkg)
            .collect()
    })
    .unwrap_or_default()
}

/// `.rpm` files (not `.src.rpm`) in `rpms_dir` modified within `window`.
/// Returns 0 when the directory doesn't exist or can't be read.
fn count_recent_rpms(rpms_dir: &std::path::Path, window: Duration) -> usize {
    let Ok(iter) = std::fs::read_dir(rpms_dir) else {
        return 0;
    };
    iter.filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".rpm") && !n.ends_with(".src.rpm")
        })
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age <= window)
        })
        .count()
}

/// Runs `osc getbinaries --destdir=<dest> <project> <package> <repo> <arch>`.
/// Errors are non-fatal: the caller logs a warning and continues.
pub fn get_binaries(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    dest_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.to_string_lossy();
    let out = proc::capture_with_timeout(
        "osc",
        &[
            "getbinaries",
            &format!("--destdir={dest}"),
            project,
            pkg_name,
            repo,
            arch,
        ],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "getbinaries {project} {pkg_name} {repo} {arch}"
    )))?;

    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("getbinaries {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Try to extract unresolvable package names (and their raw version-detail
/// lines, see [`extract_unresolvable_detail`]) from `osc results --verbose`.
///
/// Used as a fallback when the build log doesn't contain "nothing provides" lines.
fn get_unresolvable_from_results(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> (Vec<String>, Vec<String>) {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return (Vec::new(), Vec::new());
    }
    let out = match proc::capture(
        "osc",
        &[
            "results",
            "--verbose",
            "-r",
            repo,
            "-a",
            arch,
            project,
            pkg_name,
        ],
        &anywhere(),
        emit,
    ) {
        Ok(out) => out,
        Err(e) => {
            ap_core::diag!("  osc results --verbose {project}/{pkg_name}: {e}");
            return (vec![], vec![]);
        }
    };
    (
        extract_unresolvable(&out.stdout),
        extract_unresolvable_detail(&out.stdout),
    )
}

/// Get the unresolvable package names (and raw detail lines) OBS already
/// knows about for a package, without triggering a new build: reads whatever
/// build log already sits at `log_path`, falling back to `osc results
/// --verbose` when that log has no "nothing provides" lines (the same
/// fallback [`obs_build`] uses for a build it just ran). Exposed so a caller
/// that finds a package already `Unresolvable` on restart can inform the fix
/// agent immediately instead of spending an attempt rediscovering it through
/// a fresh build.
pub fn current_unresolvable(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    emit: Emit<'_>,
) -> (Vec<String>, Vec<String>) {
    // The `_result` document is what OBS says about the current revision in
    // exactly this repository and architecture; a build log may be left over
    // from an older revision, and the `osc results` text is a rendering of
    // that same document, so both only run when it gives nothing.
    if let Some(details) = unresolvable_details(project, pkg_name, repo, arch) {
        let names = extract_unresolvable(&details);
        if !names.is_empty() {
            return (names, extract_unresolvable_detail(&details));
        }
    }
    let log_text = fs::read_to_string(log_path).unwrap_or_default();
    let mut names = extract_unresolvable(&log_text);
    let mut detail = extract_unresolvable_detail(&log_text);
    if names.is_empty() {
        (names, detail) = get_unresolvable_from_results(project, pkg_name, repo, arch, emit);
    }
    (names, detail)
}

/// The `<details>` text of `pkg_name`'s `unresolvable` status in this one
/// repository/architecture, or `None` when it is not unresolvable there (or
/// the status cannot be read). Other repositories' results in the same
/// document are never looked at: a package can be `unresolvable` on `i586`
/// and `succeeded` on `x86_64` at once.
pub fn unresolvable_details(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let xml = osc_api(&format!(
        "/build/{project}/_result?package={}&repository={}&arch={}",
        enc(pkg_name),
        enc(repo),
        enc(arch)
    ))?;
    unresolvable_details_from_xml(&xml, repo, arch, pkg_name)
}

fn unresolvable_details_from_xml(xml: &str, repo: &str, arch: &str, pkg: &str) -> Option<String> {
    let entry = obs_status::find_entry(xml, repo, arch, pkg)?;
    (BuildStatus::from(entry.code.as_str()) == BuildStatus::Unresolvable)
        .then_some(entry.details)
        .flatten()
}

/// The packages that must be built before the unresolvable `pkg_name` can
/// build, read from OBS's own `nothing provides` details for this
/// repository/architecture and mapped back to package names by
/// [`missing_package_names`]. Empty when OBS names none (not unresolvable
/// here, a version conflict, or an unreadable status).
pub fn unresolvable_blockers(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Vec<String> {
    match unresolvable_details(project, pkg_name, repo, arch) {
        Some(details) => blockers_from_details(&details),
        None => Vec::new(),
    }
}

fn blockers_from_details(details: &str) -> Vec<String> {
    // A requirement with a version bound is the spec's own constraint failing
    // against the one version the repository has, which building the named
    // package cannot fix; only unconstrained ones are packages to build.
    let unconstrained: Vec<String> = details
        .lines()
        .flat_map(nothing_provides_segments)
        .filter(|segment| segment.split_whitespace().nth(1).is_none())
        .map(|segment| format!("nothing provides {segment}"))
        .collect();
    missing_package_names(&extract_unresolvable(&unconstrained.join("\n")))
}

/// The name `dep` has in `listing` when the project has it built
/// (`succeeded`) in the repository/architecture `statuses` was read for, else
/// `None`. Names are matched by [`fuzzy_key`], with or without the `python-`
/// prefix, and the listing's own spelling is returned.
pub fn built_in_project(
    listing: &std::collections::HashSet<String>,
    statuses: &HashMap<String, BuildStatus>,
    dep: &str,
) -> Option<String> {
    let bare = fuzzy_key(dep);
    let rpm = fuzzy_key(&rpm_name(dep));
    listing
        .iter()
        .filter(|p| {
            let k = fuzzy_key(p);
            k == bare || k == rpm
        })
        .find(|p| statuses.get(*p).is_some_and(BuildStatus::is_success))
        .cloned()
}

/// Section headers of an RPM spec, for [`spec_section`].
const SPEC_SECTIONS: &[&str] = &[
    "%package",
    "%description",
    "%prep",
    "%conf",
    "%generate_buildrequires",
    "%build",
    "%install",
    "%check",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%pretrans",
    "%posttrans",
    "%files",
    "%changelog",
];

/// The lines of every `name` section (`%files`, `%install`, ...) of `spec`,
/// header lines excluded.
fn spec_section<'a>(spec: &'a str, name: &str) -> Vec<&'a str> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in spec.lines() {
        let word = line.split_whitespace().next().unwrap_or("");
        if SPEC_SECTIONS.contains(&word) {
            inside = word == name;
            continue;
        }
        if inside {
            lines.push(line);
        }
    }
    lines
}

/// Whether `spec` is an unfinished skeleton whose package builds but ships
/// next to nothing, and why: a `%files` that lists only `%license`/`%doc`
/// (nothing the build installed is packaged), or an `%install` that is empty
/// apart from comments next to a `TODO`. Both are what the skeleton
/// generators leave for a monorepo or a wheel they could not lay out, and OBS
/// builds either one "successfully".
pub fn unfinished_spec_reason(spec: &str) -> Option<String> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let is_code = |l: &&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let files: Vec<&str> = spec_section(body, "%files")
        .into_iter()
        .filter(is_code)
        .collect();
    let files_listed = files.iter().any(|l| {
        let t = l.trim_start();
        !["%license", "%doc", "%defattr", "%dir"]
            .iter()
            .any(|p| t.starts_with(p))
    });
    let mut reasons = Vec::new();
    if body.contains("\n%files") && !files_listed {
        reasons.push("its %files lists nothing but %license/%doc".to_string());
    }
    if !spec_section(body, "%install").iter().any(is_code)
        && body
            .lines()
            .any(|l| l.trim_start().starts_with('#') && l.contains("TODO"))
    {
        reasons.push("its %install has no commands, only TODO comments".to_string());
    }
    (!reasons.is_empty()).then(|| reasons.join(" and "))
}

/// The `<version>` of a `/source/<project>/<package>?view=info&parse=1`
/// answer, or `None` when it names none (a package with no parsable spec).
fn parse_sourceinfo_version(xml: &str) -> Option<String> {
    let rest = &xml[xml.find("<version>")? + "<version>".len()..];
    let version = rest[..rest.find("</version>")?].trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// The version `pkg_name` has in the sources of `project`, read from the spec
/// OBS parsed, or `None` when it cannot be told. The version a linked project
/// ships is what a dependent's bound has to be met by, so this is how the
/// closure check learns it without checking the package out.
pub fn source_version(project: &str, pkg_name: &str) -> Option<String> {
    let xml = osc_api(&format!(
        "/source/{}/{}?view=info&parse=1",
        enc(project),
        enc(pkg_name)
    ))?;
    parse_sourceinfo_version(&xml)
}

#[cfg(test)]
mod sourceinfo_version_tests {
    use super::parse_sourceinfo_version;

    #[test]
    fn reads_the_version_element() {
        let xml = "<sourceinfo package=\"python-x\"><name>python-x</name>\
                   <version>0.1.14.post1</version><release>0</release></sourceinfo>";
        assert_eq!(
            parse_sourceinfo_version(xml).as_deref(),
            Some("0.1.14.post1")
        );
    }

    #[test]
    fn no_version_element_is_none() {
        assert_eq!(parse_sourceinfo_version("<sourceinfo/>"), None);
        assert_eq!(parse_sourceinfo_version("<version></version>"), None);
    }
}

/// A warning for a package OBS reports `succeeded` whose result a dependent
/// cannot use, or `None` when nothing is wrong or it cannot be told. The test
/// is deterministic: the spec OBS holds is an unfinished skeleton (see
/// [`unfinished_spec_reason`]), and the RPMs it published are listed with
/// their sizes as evidence. A dependent installs such a package and still
/// finds its modules missing, which reads as "the dependency is missing".
pub fn empty_package_warning(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
) -> Option<String> {
    let spec = osc_api(&format!(
        "/source/{}/{}/{}.spec",
        enc(project),
        enc(pkg_name),
        enc(pkg_name)
    ))?;
    let reason = unfinished_spec_reason(&spec)?;
    let sizes = osc_api(&format!(
        "/build/{}/{}/{}/{}",
        enc(project),
        enc(repo),
        enc(arch),
        enc(pkg_name)
    ))
    .map(|xml| obs_status::parse_binary_rpm_sizes(&xml))
    .unwrap_or_default();
    Some(empty_package_message(pkg_name, project, &reason, &sizes))
}

fn empty_package_message(
    pkg: &str,
    project: &str,
    reason: &str,
    sizes: &[(String, u64)],
) -> String {
    let published = if sizes.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = sizes
            .iter()
            .map(|(f, b)| format!("{f}: {b} bytes"))
            .collect();
        format!(" (published: {})", list.join(", "))
    };
    format!(
        "{pkg} is built in {project}, but its spec there is an unfinished skeleton: {reason}, \
         so the RPMs it published are effectively empty{published}. A package that needs it \
         installs it and still cannot import it. Finish {pkg}'s spec, then rebuild it with \
         `auto-package build --project {project} --force <name>`"
    )
}

/// Package names to queue for building, from the RPM names a build or an
/// agent reports missing: see [`crate::depclass::triage_all`], here without
/// asking the distro (the names come from OBS saying the target has nothing
/// providing them). Callers that want to tell the user why a name was left out
/// use `triage_all` directly.
pub fn missing_package_names(rpm_names: &[String]) -> Vec<String> {
    crate::depclass::triage_all(rpm_names, &|_| false).0
}

/// Whether `name` is an RPM capability (a shared library such as
/// `libcuda.so.1()(64bit)`, a file path, an `rpmlib(...)` feature) rather than
/// a package name. OBS reports these as `nothing provides` too, but there is
/// no project to build for them: `libcuda.so.1` is the NVIDIA driver's own
/// library (convention 60), supplied at runtime, so queueing it as a package
/// can never succeed. The fix is on the package that requires it (an
/// `%global __requires_exclude`), which convention 60 tells the agent.
pub fn is_rpm_capability(name: &str) -> bool {
    let name = name.trim();
    if name.starts_with('/') || name.starts_with("rpmlib(") || name.contains("()") {
        return true;
    }
    // `lib<x>.so`, `.so.<n>`: not a name that merely contains ".so".
    name.match_indices(".so")
        .any(|(i, _)| matches!(name[i + 3..].chars().next(), None | Some('.') | Some('(')))
}

/// Whether `info` (a project meta) builds for `repo` on `arch`: the `Err`
/// says what the project does offer. A meta with no repository at all is
/// left to the "no build repositories" handling elsewhere.
fn check_repo_arch(
    info: &ProjectInfo,
    project: &str,
    repo: &str,
    arch: &str,
) -> Result<(), String> {
    if info.repositories.is_empty() {
        return Ok(());
    }
    let Some(def) = info.repositories.iter().find(|r| r.name == repo) else {
        let names: Vec<&str> = info.repositories.iter().map(|r| r.name.as_str()).collect();
        return Err(format!(
            "{project} has no repository {repo} (it has: {})",
            names.join(", ")
        ));
    };
    if !def.arches.iter().any(|a| a == arch) {
        return Err(format!(
            "{project} does not build {repo} for {arch} (it builds: {})",
            def.arches.join(", ")
        ));
    }
    Ok(())
}

/// Fail before waiting when the project does not build for `repo`/`arch`:
/// OBS would answer with no result at all, forever. A meta that cannot be
/// read skips the check; the wait's own "no results" limit still applies.
fn ensure_builds_for(project: &str, repo: &str, arch: &str) -> Result<(), Error> {
    let Ok(Some(meta)) = get_project_meta(project, Emit::quiet()) else {
        return Ok(());
    };
    check_repo_arch(&parse_project_info(&meta), project, repo, arch).map_err(Error::NotActionable)
}

/// How long to wait before the next status poll. A status that keeps
/// holding is polled less often (15s, then 30s after 4 unchanged polls, then
/// 60s after 8) and a failing poll backs off exponentially up to 2 minutes, so
/// a long build or an ailing server is not hammered. `jitter` in `[0, 1)`
/// spreads concurrent waiters by up to +-20%.
fn poll_delay(unchanged_polls: u32, errors: u32, jitter: f64) -> Duration {
    let base = match unchanged_polls {
        0..=3 => 15,
        4..=7 => 30,
        _ => 60,
    };
    let secs = if errors == 0 {
        base
    } else {
        (base << errors.min(4)).min(120)
    };
    Duration::from_secs_f64(secs as f64 * (0.8 + 0.4 * jitter))
}

/// The unchanged-poll count that sets the delay while waiting for a build. A
/// status that only awaits the new revision (see
/// [`obs_status::awaiting_new_revision`]) is not "holding still": the build is
/// about to start, so it never backs off past the 30s tier.
fn wait_polls(unchanged_polls: u32, awaiting_new_revision: bool) -> u32 {
    if awaiting_new_revision {
        unchanged_polls.min(7)
    } else {
        unchanged_polls
    }
}

/// How long an unchanged status holds before the independent `osc results`
/// cross-check, asked once per stuck period (see [`recheck_due`]).
const NO_PROGRESS_RECHECK: Duration = Duration::from_secs(300);
// A `stale`/`unconfirmed` status is the previous revision's verdict served
// while OBS has not taken up the commit (or has, and the document has not
// caught up): the independent read says which, so it is asked early and again
// at this pace, instead of once after minutes.
const STALE_RECHECK_FIRST: Duration = Duration::from_secs(30);
const STALE_RECHECK_EVERY: Duration = Duration::from_secs(60);

/// Whether the independent `osc results` cross-check of a non-terminal status
/// is due: `since_change` is how long the status has not changed,
/// `since_recheck` how long ago the last check of this stuck period was. A
/// status that awaits the new revision is checked after 30s and then every
/// 60s; any other unchanged status once, after 300s.
fn recheck_due(
    awaiting_new_revision: bool,
    since_change: Duration,
    since_recheck: Option<Duration>,
) -> bool {
    match (awaiting_new_revision, since_recheck) {
        (true, None) => since_change >= STALE_RECHECK_FIRST,
        (true, Some(last)) => last >= STALE_RECHECK_EVERY,
        (false, None) => since_change >= NO_PROGRESS_RECHECK,
        (false, Some(_)) => false,
    }
}

/// A pseudo-random value in `[0, 1)` from the clock, enough to de-synchronise
/// pollers without a randomness dependency.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
}

/// The `<details>` OBS gives a package's result in `repo`/`arch` (what a
/// `blocked` or `scheduled` package is waiting for), if any.
fn build_details(project: &str, pkg_name: &str, repo: &str, arch: &str) -> Option<String> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return None;
    }
    let entries = obs_package_entries(project, repo, arch, pkg_name, None)
        .map_err(|e| match e {
            ApiFailure::Http(_, m) | ApiFailure::Other(m) => ap_core::diag!("  {m}"),
        })
        .ok()??;
    obs_status::entry_in(&entries, repo, arch, pkg_name)?
        .details
        .clone()
}

/// Sleep for `total`, waking each second to notice a shutdown request, so a
/// long poll delay does not hold up an interrupt.
fn sleep_interruptible(total: Duration) {
    let end = Instant::now() + total;
    while !crate::proc::shutting_down() {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_secs(1)));
    }
}

/// Wait for the OBS remote build to complete, fetch the log, and download
/// built RPMs on success.
///
/// The caller must commit the package to OBS before calling this (see `commit`).
/// Polls the OBS build status (every 15 seconds, less often while it holds
/// still; see [`poll_delay`]), for at most two hours (the
/// `MAX_WAIT` below), then gives up with a timeout error. Emits a `Progress`
/// event each time the build status changes so the user can see progress.
pub fn obs_build(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    build_with_retriggers(
        pkg_name,
        emit,
        BROKEN_RETRIGGER_PAUSE,
        || obs_build_once(project, pkg_name, repo, arch, log_path, rpms_dir, emit),
        || trigger_rebuild(project, pkg_name, repo, arch, emit),
    )
}

/// The retry loop of [`obs_build`], with the build and the re-trigger injected:
/// a `broken` verdict with no build log behind it is re-triggered up to
/// [`MAX_BROKEN_RETRIGGERS`] times, then stands as a plain `broken`. Any other
/// result is returned as it came.
fn build_with_retriggers(
    pkg_name: &str,
    emit: Emit<'_>,
    pause: Duration,
    mut once: impl FnMut() -> Result<(), Error>,
    mut rebuild: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    let mut retriggers = 0;
    loop {
        match once() {
            Err(Error::NotActionable(msg)) if msg == BROKEN_UNCONFIRMED => {
                if retriggers >= MAX_BROKEN_RETRIGGERS {
                    return Err(Error::NotActionable(BROKEN_MESSAGE.to_string()));
                }
                retriggers += 1;
                emit.reporter.report(&Event::Warning {
                    step: emit.step,
                    message: format!(
                        "OBS says {pkg_name} is broken but has no build log to confirm it; \
                         treating it as transient and re-triggering the build \
                         ({retriggers}/{MAX_BROKEN_RETRIGGERS})"
                    ),
                });
                if let Err(e) = rebuild() {
                    emit.reporter.report(&Event::Warning {
                        step: emit.step,
                        message: format!("could not re-trigger the OBS build: {e}"),
                    });
                }
                if !pause.is_zero() {
                    sleep_interruptible(pause);
                }
            }
            other => return other,
        }
    }
}

/// The final message for a `broken` verdict.
const BROKEN_MESSAGE: &str = "OBS: package source is broken";
/// Internal marker: `broken` without a build log behind it (see [`obs_build`]).
const BROKEN_UNCONFIRMED: &str = "OBS: broken status could not be confirmed by a build log";
/// How often an unconfirmed `broken` verdict is re-triggered before it stands.
const MAX_BROKEN_RETRIGGERS: u32 = 3;
const BROKEN_RETRIGGER_PAUSE: Duration = Duration::from_secs(30);

/// Whether a `broken` verdict is backed by evidence: a non-empty build log.
fn broken_is_confirmed(log_path: &Path) -> bool {
    fs::read_to_string(log_path).is_ok_and(|t| !t.trim().is_empty())
}

/// Ask OBS to rebuild `pkg_name` (`osc rebuild`), which re-evaluates a source
/// it had flagged `broken`.
fn trigger_rebuild(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    emit: Emit<'_>,
) -> Result<(), Error> {
    check_obs_name("project", project)?;
    check_obs_name("pkg_name", pkg_name)?;
    check_obs_name("repo", repo)?;
    check_obs_name("arch", arch)?;
    let out = proc::capture_with_timeout(
        "osc",
        &["rebuild", project, pkg_name, repo, arch],
        &anywhere(),
        emit,
        OSC_ARTIFACT_TIMEOUT,
    )
    .map_err(during(format!(
        "rebuild {project} {pkg_name} {repo} {arch}"
    )))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("rebuild {project} {pkg_name} {repo} {arch}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// Call `f` up to `attempts` times, pausing `pause` between failures.
fn retry<T, E>(
    attempts: u32,
    pause: Duration,
    mut f: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    let mut n = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if n >= attempts => return Err(e),
            Err(_) => {
                n += 1;
                if !pause.is_zero() {
                    sleep_interruptible(pause);
                }
            }
        }
    }
}

/// `osc getbinaries` right after a green verdict can fail transiently.
const GETBINARIES_ATTEMPTS: u32 = 4;
const GETBINARIES_PAUSE: Duration = Duration::from_secs(20);

/// One package's `getbinaries`, `get` injected: retried up to `attempts` times,
/// and each try waits for free disk space first (a full disk fails the try with
/// code 28 without running `get`).
#[allow(clippy::too_many_arguments)]
fn fetch_with_retries(
    project: &str,
    name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &Path,
    emit: Emit<'_>,
    attempts: u32,
    pause: Duration,
    mut get: impl FnMut() -> Result<(), Error>,
) -> Result<(), Error> {
    retry(attempts, pause, || {
        if let Err(stderr) = crate::workspace::await_disk_space(rpms_dir, emit.reporter, emit.step)
        {
            return Err(Error::OscFailed {
                cmd: format!("getbinaries {project} {name} {repo} {arch}"),
                code: 28,
                stderr,
            });
        }
        get()
    })
}

/// Download the RPMs of `pkg_name` (and its multibuild flavors) into
/// `rpms_dir`, retrying a failing `osc getbinaries`. Returns how many of the
/// RPMs OBS lists landed.
pub fn download_built_rpms(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<usize, Error> {
    let mut packages = vec![pkg_name.to_string()];
    packages.extend(multibuild_flavors(project, pkg_name, repo, arch));
    let mut downloaded_count = 0;
    for name in &packages {
        let listing = binary_rpm_listing(project, name, repo, arch);
        if let Some(names) = &listing {
            remove_superseded_rpms(rpms_dir, names);
        }
        fetch_with_retries(
            project,
            name,
            repo,
            arch,
            rpms_dir,
            emit,
            GETBINARIES_ATTEMPTS,
            GETBINARIES_PAUSE,
            || get_binaries(project, name, repo, arch, rpms_dir, emit),
        )?;
        stash_rpmlint_log(rpms_dir, name);
        downloaded_count += downloaded_rpm_count(listing.as_deref(), rpms_dir);
    }
    Ok(downloaded_count)
}

/// Directory under `rpms_dir` holding the `rpmlint.log` OBS published for each
/// package (and multibuild flavor) downloaded, until the pipeline records it.
const RPMLINT_STASH: &str = ".rpmlint";

/// `osc getbinaries` writes OBS's `rpmlint.log` next to the RPMs, where the next
/// package's download overwrites it. Move it to a file of its own for `name`.
fn stash_rpmlint_log(rpms_dir: &Path, name: &str) {
    let log = rpms_dir.join("rpmlint.log");
    if !log.is_file() {
        return;
    }
    let dir = rpms_dir.join(RPMLINT_STASH);
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::rename(&log, dir.join(format!("{name}.log")));
    }
}

/// The stashed rpmlint logs of `pkg` and its multibuild flavors
/// (`pkg:flavor`), as `(name, text)`, removed from the stash.
pub fn take_rpmlint_logs(rpms_dir: &Path, pkg: &str) -> Vec<(String, String)> {
    let Ok(rd) = fs::read_dir(rpms_dir.join(RPMLINT_STASH)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".log") else {
            continue;
        };
        if (name == pkg || name.strip_prefix(pkg).is_some_and(|r| r.starts_with(':')))
            // Bytes, lossily: a log with one invalid byte is still a log, and a
            // file that never reads would stay in the stash for ever.
            && let Ok(bytes) = fs::read(e.path())
        {
            let _ = fs::remove_file(e.path());
            out.push((
                name.to_string(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
    }
    out.sort();
    out
}

/// The error lines of an rpmlint log (`pkg.rpm: E: rule detail`).
pub fn rpmlint_errors(log: &str) -> Vec<String> {
    log.lines()
        .filter(|l| l.contains(": E: "))
        .map(|l| l.trim().to_string())
        .collect()
}

fn obs_build_once(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    log_path: &Path,
    rpms_dir: &Path,
    emit: Emit<'_>,
) -> Result<(), Error> {
    let reporter = emit.reporter;
    let step = emit.step;
    // The caller has committed already: no `_result` document fetched before
    // this may be used for the status (see `get_build_status_after_commit`).
    let committed = Instant::now();
    ensure_builds_for(project, repo, arch)?;
    const MAX_WAIT: Duration = Duration::from_secs(7200);
    // Time spent `blocked` (waiting for other packages to build) does not
    // count against MAX_WAIT while the blocker is visibly moving, i.e. its
    // details changed within BLOCK_STALL; the wait as a whole is still
    // bounded by MAX_TOTAL_WAIT.
    const BLOCK_STALL: Duration = Duration::from_secs(600);
    const MAX_TOTAL_WAIT: Duration = Duration::from_secs(6 * 3600);

    let start = Instant::now();
    let mut prev_status = String::new();

    // A single `osc` hiccup (network blip, OBS 5xx, rate limit) during a
    // build that can run for many minutes must not abort the wait and throw
    // the build away: tolerate a few consecutive poll failures, then give up.
    const MAX_CONSECUTIVE_POLL_ERRORS: u32 = 8;
    let mut poll_errors: u32 = 0;
    // Polls in a row that saw the same status; the delay between polls grows
    // with it (see `poll_delay`).
    let mut unchanged_polls: u32 = 0;

    // The previous mechanism only ever broke silence for one specific case
    // (an `Unknown("stale: ...")` verdict). A plain non-terminal status
    // (`building`, `scheduled`, ...) that simply never changes again —
    // because the XML `_result` document this reads is serving a stale
    // snapshot while OBS has actually moved on — produced no `Progress`
    // event (nothing to report: the string didn't change) and no `Warning`
    // either, so the wait looked indistinguishable from a healthy long
    // build for up to the full `MAX_WAIT`. Reported live: the TUI sat on an
    // unchanging "building" for 1000+ seconds while OBS itself showed
    // nothing in progress.
    const NO_PROGRESS_WARNING: Duration = Duration::from_secs(120);
    let mut last_change = Instant::now();
    let mut last_warning = Instant::now();
    let mut last_recheck: Option<Instant> = None;
    let mut last_table_label = String::new();
    const UNRESOLVABLE_CONFIRM: Duration = Duration::from_secs(15);
    let mut unresolvable_since: Option<Instant> = None;
    // A repository/arch the project does not build for has no result at all,
    // which would otherwise read as "not started yet" until MAX_WAIT. A
    // freshly committed package usually shows up within seconds, but a busy
    // scheduler can take minutes: after this long the project meta is
    // consulted again, and only a repository/arch it lacks ends the wait.
    const NO_RESULTS_LIMIT: Duration = Duration::from_secs(600);
    let mut no_results_since: Option<Instant> = None;
    // A built verdict whose job history cannot be read at all is accepted
    // after this long (see `obs_status::accept_unconfirmed`).
    const UNCONFIRMED_LIMIT: Duration = Duration::from_secs(300);
    let mut unconfirmed_since: Option<Instant> = None;
    let mut blocked_credit = Duration::ZERO;
    let mut last_tick = Instant::now();
    let mut last_block_change = Instant::now();
    let mut was_blocked = false;

    let status = loop {
        if crate::proc::shutting_down() {
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: "interrupted".to_string(),
            });
        }
        let tick = last_tick.elapsed();
        last_tick = Instant::now();
        if was_blocked && last_block_change.elapsed() < BLOCK_STALL {
            blocked_credit += tick;
        }
        let waited = start.elapsed();
        if waited.saturating_sub(blocked_credit) >= MAX_WAIT || waited >= MAX_TOTAL_WAIT {
            // Which limit tripped: the total one only when it did so alone.
            let limit = if waited >= MAX_TOTAL_WAIT {
                MAX_TOTAL_WAIT
            } else {
                MAX_WAIT
            };
            return Err(Error::OscFailed {
                cmd: format!("wait {project}/{pkg_name}"),
                code: -1,
                stderr: format!(
                    "timed out after {}s (limit {}s) waiting for OBS build (last status: {})",
                    waited.as_secs(),
                    limit.as_secs(),
                    if prev_status.is_empty() {
                        "none"
                    } else {
                        &prev_status
                    }
                ),
            });
        }

        let status = match get_build_status_after_commit(
            project,
            pkg_name,
            repo,
            arch,
            committed,
            Emit::quiet(),
        ) {
            Ok(s) => {
                poll_errors = 0;
                s
            }
            Err(e) => {
                poll_errors += 1;
                if poll_errors >= MAX_CONSECUTIVE_POLL_ERRORS {
                    return Err(Error::OscFailed {
                        cmd: format!("wait {project}/{pkg_name} ({repo}/{arch})"),
                        code: -1,
                        stderr: format!(
                            "gave up after {poll_errors} consecutive failed status polls; last error: {e}"
                        ),
                    });
                }
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS status poll failed ({poll_errors}/{MAX_CONSECUTIVE_POLL_ERRORS}), retrying: {e}"
                    ),
                });
                sleep_interruptible(poll_delay(unchanged_polls, poll_errors, jitter()));
                continue;
            }
        };
        if status == BuildStatus::Unknown("no results".to_string()) {
            let since = *no_results_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= NO_RESULTS_LIMIT {
                // The project meta decides: a repository/arch it does not
                // build is a dead end, anything else is a slow scheduler
                // and MAX_WAIT bounds the wait.
                ensure_builds_for(project, repo, arch)?;
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS has reported no build result for {pkg_name} on {repo}/{arch} \
                         for {}s although {project} builds for it; the scheduler is slow, \
                         still waiting",
                        since.elapsed().as_secs()
                    ),
                });
                no_results_since = Some(Instant::now());
            }
        } else {
            no_results_since = None;
        }
        // A verdict no job history could confirm (empty or evicted history,
        // failing API calls) is not worth waiting on forever: settle for it.
        let status = match obs_status::accept_unconfirmed(&status) {
            Some(accepted) => {
                let since = *unconfirmed_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= UNCONFIRMED_LIMIT {
                    reporter.report(&Event::Warning {
                        step,
                        message: format!(
                            "no OBS job history to confirm the {accepted} result for \
                             {pkg_name} after {}s; accepting it unconfirmed",
                            since.elapsed().as_secs()
                        ),
                    });
                    accepted
                } else {
                    status
                }
            }
            None => {
                unconfirmed_since = None;
                status
            }
        };
        let status_str = status.to_string();
        // What blocks or delays the package is part of the status: a change
        // in it is progress worth showing, and keeps the polling brisk.
        let label = match status {
            BuildStatus::Blocked | BuildStatus::Scheduled => {
                match build_details(project, pkg_name, repo, arch) {
                    Some(d) => format!("{status_str}: {d}"),
                    None => status_str.clone(),
                }
            }
            _ => status_str.clone(),
        };
        was_blocked = status == BuildStatus::Blocked;

        let mut status = status;
        if label == prev_status {
            unchanged_polls = unchanged_polls.saturating_add(1);
        } else {
            unchanged_polls = 0;
        }
        if label != prev_status {
            reporter.report(&Event::Progress {
                step,
                message: format!("OBS build status: {label}"),
            });
            if was_blocked {
                last_block_change = Instant::now();
            }
            prev_status = label;
            last_change = Instant::now();
            last_warning = Instant::now();
            last_recheck = None;
        } else if !status.is_terminal() {
            // Non-terminal and unchanged: make sure the user can always
            // tell this is still alive and polling, not silently wedged.
            if last_warning.elapsed() >= NO_PROGRESS_WARNING {
                reporter.report(&Event::Warning {
                    step,
                    message: format!(
                        "OBS build status still {prev_status} after {}s with no change",
                        last_change.elapsed().as_secs()
                    ),
                });
                last_warning = Instant::now();
            }
            // An independent cross-check, via a different OBS endpoint than
            // whichever one produced `status` (see
            // `get_build_status_from_results_table`'s doc comment) — catches
            // one of them silently serving a stale snapshot while the other
            // has already moved on. Once per stuck period, or, while the
            // status only says the newest sources are not built yet, soon
            // and then every minute (see `recheck_due`).
            if recheck_due(
                obs_status::awaiting_new_revision(&status),
                last_change.elapsed(),
                last_recheck.map(|t| t.elapsed()),
            ) {
                last_recheck = Some(Instant::now());
                if let Ok(table_status) = get_build_status_from_results_table(
                    project,
                    pkg_name,
                    repo,
                    arch,
                    Emit::quiet(),
                ) {
                    let table_str = table_status.to_string();
                    match obs_status::cross_check(&status, &table_status) {
                        obs_status::CrossCheck::Agrees | obs_status::CrossCheck::Ignore => {}
                        obs_status::CrossCheck::NewRevisionSeen => {
                            // The scheduler works on the new revision: the
                            // wait is progressing, so it polls briskly again.
                            unchanged_polls = 0;
                            if table_str != last_table_label {
                                reporter.report(&Event::Progress {
                                    step,
                                    message: format!(
                                        "OBS has taken up the new revision: {table_str}"
                                    ),
                                });
                                last_table_label = table_str;
                            }
                        }
                        obs_status::CrossCheck::Adopt => {
                            reporter.report(&Event::Warning {
                                step,
                                message: format!(
                                    "OBS status disagreement after {}s of no change: the API \
                                     document says {status_str}, but `osc results` says \
                                     {table_str} — trusting the independent read",
                                    last_change.elapsed().as_secs()
                                ),
                            });
                            reporter.report(&Event::Progress {
                                step,
                                message: format!("OBS build status: {table_str}"),
                            });
                            prev_status = table_str;
                            last_change = Instant::now();
                            last_warning = Instant::now();
                            last_recheck = None;
                            unchanged_polls = 0;
                            status = table_status;
                        }
                    }
                }
            }
        }

        // `unresolvable` is only the scheduler's current view: a dependency
        // published a moment later turns it back into `scheduled`. Accept it
        // as the answer only once a later poll agrees (UNRESOLVABLE_CONFIRM is
        // one poll interval, so a genuine verdict costs a single extra poll).
        if status == BuildStatus::Unresolvable {
            let since = *unresolvable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= UNRESOLVABLE_CONFIRM {
                break status;
            }
        } else {
            unresolvable_since = None;
            if status.is_terminal() {
                break status;
            }
        }

        // `stale: <verdict>` is not a final answer: that terminal result is
        // from a source revision the server may already have superseded, so
        // keep polling. Only a verdict confirmed for the current sources
        // (see `obs_status::resolve`) ends the wait; if none arrives, the
        // MAX_WAIT check above reports a timeout error.
        sleep_interruptible(poll_delay(
            wait_polls(unchanged_polls, obs_status::awaiting_new_revision(&status)),
            0,
            jitter(),
        ));
    };

    // Fetch the log regardless of outcome — partial logs help diagnose failures.
    // A missing or stale log leaves the failure without its cause, so say so.
    // Announce the command that really writes `log_path`; the polling above
    // goes through the API and runs no `osc results --watch`.
    //
    // A multibuild package whose own entry built but whose flavor did not has
    // the flavor's log as the evidence: the package's would show a success and
    // send the fix agent after a failure it cannot see.
    let log_pkg = if status.is_success() {
        pkg_name.to_string()
    } else {
        failing_result(project, pkg_name, repo, arch, &status)
    };
    if log_pkg != pkg_name {
        reporter.report(&Event::Warning {
            step,
            message: format!(
                "{pkg_name} itself built, but its multibuild flavor {log_pkg} did not: \
                 the build log is {log_pkg}'s"
            ),
        });
    }
    reporter.report(&Event::Command {
        step,
        program: "osc".to_string(),
        argv: vec![
            "buildlog".to_string(),
            project.to_string(),
            log_pkg.clone(),
            repo.to_string(),
            arch.to_string(),
        ],
        cwd: anywhere(),
        log: Some(log_path.to_path_buf()),
    });
    if let Err(e) = fetch_buildlog(project, &log_pkg, repo, arch, log_path, Emit::quiet()) {
        reporter.report(&Event::Warning {
            step,
            message: format!("could not fetch the OBS build log for {log_pkg}: {e}"),
        });
    }
    let log_bytes = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    let elapsed = start.elapsed();

    if status.is_success() {
        // Download the built RPMs. The Oct 6 review's reproduction of
        // 'succeeded status followed by a failing getbinaries still
        // returns success, leaving the RPM directory empty' turned on
        // this call being non-fatal: a succeeded OBS build without any
        // locally-downloaded artifacts is NOT a success a dependent
        // build can rely on — `rpm_satisfies_dep` can't find anything,
        // the dep is reported unresolvable even though OBS says the
        // source is green, and the dependent spends attempts chasing a
        // nonexistent problem. Promote this to fatal: if OBS says
        // succeeded but we cannot pull the binaries, surface that as a
        // build failure.
        //
        // We also verify that at least one `.rpm` landed under
        // `rpms_dir` after the fetch — `osc getbinaries` can exit 0
        // with no files written when the project is misconfigured
        // (empty binary set for this repo/arch pair), which the warning
        // version silently papered over.
        let fetch_result = download_built_rpms(project, pkg_name, repo, arch, rpms_dir, emit);
        let downloaded_count = fetch_result.as_ref().copied().unwrap_or(0);
        if let Err(e) = fetch_result {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            let advice = if e.to_string().contains("the disk is full") {
                // The build is fine; only this machine could not take the files.
                "The build itself succeeded: free disk space and run again."
            } else {
                "A green source without downloadable artifacts is not a usable \
                 build — a dependent package would see every BuildRequires as \
                 unresolvable. Re-check the OBS project's binary publishing config \
                 for repo={repo}/arch={arch}."
            };
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} {DOWNLOAD_FAILED}: {e}. {}",
                advice.replace("{repo}", repo).replace("{arch}", arch)
            )));
        }
        if downloaded_count == 0 {
            reporter.report(&Event::CommandDone {
                step,
                code: 1,
                elapsed,
                stdout_bytes: log_bytes,
                stderr_bytes: 0,
            });
            return Err(Error::NotActionable(format!(
                "OBS reported {pkg_name} succeeded and `osc getbinaries` exited 0, \
                 but no .rpm files appeared under {}. The project's binary \
                 publishing for repo={repo}/arch={arch} is probably misconfigured \
                 (empty binary set) — check the OBS <publish>/<binaries> settings.",
                rpms_dir.display()
            )));
        }
        reporter.report(&Event::CommandDone {
            step,
            code: 0,
            elapsed,
            stdout_bytes: log_bytes,
            stderr_bytes: 0,
        });
        return Ok(());
    }

    reporter.report(&Event::CommandDone {
        step,
        code: 1,
        elapsed,
        stdout_bytes: log_bytes,
        stderr_bytes: 0,
    });

    Err(match status {
        BuildStatus::Unresolvable => {
            let (names, detail) =
                current_unresolvable(project, &log_pkg, repo, arch, log_path, emit);
            Error::UnresolvableDeps { names, detail }
        }
        BuildStatus::Broken if !broken_is_confirmed(log_path) => {
            Error::NotActionable(BROKEN_UNCONFIRMED.to_string())
        }
        BuildStatus::Broken => {
            Error::NotActionable(match build_details(project, &log_pkg, repo, arch) {
                Some(d) => format!("{BROKEN_MESSAGE}: {d}"),
                None => BROKEN_MESSAGE.to_string(),
            })
        }
        BuildStatus::Disabled => {
            Error::NotActionable("OBS: build is disabled in project config".to_string())
        }
        BuildStatus::Excluded => {
            Error::NotActionable("OBS: build is excluded in project config".to_string())
        }
        _ => {
            // The log is the only evidence for what went wrong. One that
            // cannot be read, or is empty (a failed `osc buildlog` leaves an
            // empty file behind), is an environment problem: classifying it
            // would send the package to the fix agent as a BuildFailed.
            let log_text = fs::read_to_string(log_path)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    if text.trim().is_empty() {
                        Err("the file is empty".to_string())
                    } else {
                        Ok(text)
                    }
                })
                .map_err(|why| {
                    Error::Io(std::io::Error::other(format!(
                        "no usable build log at {}: {why}",
                        log_path.display()
                    )))
                })?;
            match classify_build_failure(&log_text) {
                FailureKind::NoRepositories => Error::NotActionable(
                    "OBS project has no build repositories — configure them in OBS first"
                        .to_string(),
                ),
                FailureKind::NotWorkingCopy => {
                    Error::NotActionable("osc checkout is not server-backed".to_string())
                }
                FailureKind::UnresolvableDeps { names, detail } => {
                    Error::UnresolvableDeps { names, detail }
                }
                FailureKind::BuildFailed => Error::BuildFailed {
                    spec: format!("{pkg_name}.spec"),
                    code: 1,
                    log: log_path.to_owned(),
                },
            }
        }
    })
}

#[cfg(test)]
mod package_listing_tests {
    use super::*;

    #[test]
    fn only_oscs_own_up_to_date_lines_are_benign() {
        assert!(is_benign_up_output("At revision 12.\n"));
        assert!(is_benign_up_output("nothing to update"));
        assert!(!is_benign_up_output(""));
        assert!(!is_benign_up_output("Skipping foo: conflict"));
        assert!(!is_benign_up_output("Error: At revision 3. failed\n"));
        assert!(!is_benign_up_output("At revision 12.\nTraceback: boom"));
    }

    #[test]
    fn get_binaries_reports_an_uncreatable_destination() {
        // A regular file in the path makes `create_dir_all` fail before any
        // `osc` call, which used to be ignored.
        let file = std::env::temp_dir().join(format!("ap-osc-notdir-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let r = get_binaries("p", "pkg", "r", "a", &file.join("rpms"), Emit::quiet());
        std::fs::remove_file(&file).ok();
        assert!(r.is_err());
    }

    #[test]
    fn a_python_multibuild_flavor_counts_as_an_available_package() {
        let pkgs =
            parse_package_listing("apache-arrow\napache-arrow:python-pyarrow\nPython-Foo\n\n");
        assert!(pkgs.contains("python-pyarrow"));
        assert!(pkgs.contains("apache-arrow:python-pyarrow"));
        assert!(pkgs.contains("apache-arrow"));
        assert!(pkgs.contains("Python-Foo"));
    }

    #[test]
    fn a_non_python_multibuild_flavor_is_not_promoted_to_a_package() {
        let pkgs = parse_package_listing("gcc15:bootstrap\nfoo:test\n");
        assert!(!pkgs.contains("bootstrap"));
        assert!(!pkgs.contains("test"));
    }

    /// OBS names are case-sensitive: the listing must keep the spelling it
    /// has, or the name passed back to osc points at a package that does not
    /// exist (`python-apscheduler` for `python-APScheduler`).
    #[test]
    fn the_listing_keeps_each_package_in_its_own_case() {
        let pkgs = parse_package_listing("python-APScheduler\n");
        assert!(pkgs.contains("python-APScheduler"));
        assert!(!pkgs.contains("python-apscheduler"));
    }

    #[test]
    fn obs_name_returns_the_listed_spelling_for_a_lowercase_guess() {
        let listing = parse_package_listing("python-APScheduler\npython-pydantic_core\n");
        assert_eq!(
            obs_name(&listing, "python-apscheduler"),
            "python-APScheduler"
        );
        // A separator difference between the guess and the listing still finds it.
        assert_eq!(
            obs_name(&listing, "python-pydantic-core"),
            "python-pydantic_core"
        );
    }

    #[test]
    fn obs_name_keeps_the_guess_when_nothing_is_listed() {
        let listing = parse_package_listing("python-APScheduler\n");
        assert_eq!(obs_name(&listing, "python-croniter"), "python-croniter");
        assert_eq!(
            obs_name(&listing, "python-APScheduler"),
            "python-APScheduler"
        );
    }

    #[test]
    fn obs_name_is_stable_when_several_spellings_match() {
        let listing = parse_package_listing("python-Foo\npython-foo\n");
        assert_eq!(obs_name(&listing, "python-FOO"), "python-Foo");
    }
}

#[cfg(test)]
mod obs_name_tests {
    use super::*;

    #[test]
    fn obs_names_reject_options_paths_and_junk() {
        for ok in [
            "python-Events",
            "libsigc++",
            "home:dspinella:python3",
            "a_b.c-1",
        ] {
            assert!(is_valid_obs_name(ok), "{ok}");
        }
        for bad in ["", "-rf", "../x", "a/b", "a b", "a\nb", "x..y", "/abs"] {
            assert!(!is_valid_obs_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_capability_that_looks_like_an_option_is_never_queried() {
        // Returns before touching zypper or the cache.
        assert!(!distro_provides("--repo"));
        assert!(!distro_provides(" -x"));
    }

    #[test]
    fn xml_escape_covers_attribute_metacharacters() {
        assert_eq!(xml_escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
    }
}

#[cfg(test)]
mod count_rpms_tests {
    use super::*;

    #[test]
    fn recent_rpms_exclude_source_rpms_and_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("python313-Events-0.5-2.1.noarch.rpm"), b"").unwrap();
        std::fs::write(tmp.path().join("python-events-0.5-2.1.src.rpm"), b"").unwrap();
        std::fs::write(tmp.path().join("_buildenv"), b"").unwrap();
        assert_eq!(count_recent_rpms(tmp.path(), Duration::from_secs(60)), 1);
    }

    #[test]
    fn recent_rpms_tolerates_missing_dir() {
        assert_eq!(
            count_recent_rpms(
                std::path::Path::new("/does/not/exist"),
                Duration::from_secs(60)
            ),
            0
        );
    }
}

#[cfg(test)]
mod project_build_statuses_tests {
    use super::*;

    /// `/build/home:dspinella:open-webui/_result` as OBS returned it for the
    /// three dependencies of the vllm run, trimmed to their statuses: every
    /// one is fine on x86_64 and broken or excluded on i586.
    const TWO_ARCHES: &str = r#"<resultlist state="a89fd62d90378c6fa375e52c78300d1c">
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-fastapi-cloud-cli" code="unresolvable">
      <details>nothing provides python314-fastar, nothing provides python313-fastar</details>
    </status>
    <status package="python-nvidia-cutlass-dsl-libs-base" code="excluded"/>
    <status package="python-cuda-bindings" code="succeeded"/>
  </result>
  <result project="home:dspinella:open-webui" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building">
    <status package="python-fastapi-cloud-cli" code="succeeded"/>
    <status package="python-nvidia-cutlass-dsl-libs-base" code="succeeded"/>
    <status package="python-cuda-bindings" code="succeeded"/>
  </result>
</resultlist>"#;

    #[test]
    fn a_package_excluded_on_one_arch_is_not_excluded_on_the_one_being_built() {
        let x86 = project_statuses_from_xml(TWO_ARCHES, "openSUSE_Tumbleweed", "x86_64").unwrap();
        assert_eq!(x86.len(), 3);
        assert!(x86.values().all(|s| *s == BuildStatus::Succeeded));
        let i586 = project_statuses_from_xml(TWO_ARCHES, "openSUSE_Tumbleweed", "i586").unwrap();
        assert_eq!(
            i586.get("python-nvidia-cutlass-dsl-libs-base"),
            Some(&BuildStatus::Excluded)
        );
        assert_eq!(
            i586.get("python-fastapi-cloud-cli"),
            Some(&BuildStatus::Unresolvable)
        );
        assert_eq!(
            i586.get("python-cuda-bindings"),
            Some(&BuildStatus::Succeeded)
        );
    }

    #[test]
    fn a_repository_or_arch_the_project_does_not_build_for_yields_nothing() {
        for (repo, arch) in [
            ("openSUSE_Tumbleweed", "aarch64"),
            ("openSUSE_Leap_16.0", "x86_64"),
        ] {
            assert!(
                project_statuses_from_xml(TWO_ARCHES, repo, arch)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn a_cut_off_listing_is_an_error_not_a_partial_map() {
        let cut = &TWO_ARCHES[..TWO_ARCHES.find("arch=\"x86_64\"").unwrap() + 20];
        assert!(project_statuses_from_xml(cut, "openSUSE_Tumbleweed", "i586").is_err());
    }

    /// Regression test for the off-by-one that used to read the separator
    /// space between status codes instead of the code itself, silently
    /// returning no statuses at all and making every OBS-known package look
    /// "resolvable" regardless of its actual build state.
    #[test]
    fn reads_status_codes_at_even_columns_not_the_separators() {
        let stdout = "\
python-foo
python-bar
python-baz
. B U openSUSE_Tumbleweed x86_64 (published)
Legend:
 . succeeded
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), Some(&BuildStatus::Broken));
        assert_eq!(statuses.get("python-baz"), Some(&BuildStatus::Unresolvable));
    }

    /// Regression test for the padded real-world format seen from actual
    /// `osc results` output on a project with many packages: a leading
    /// separator before the first code and a trailing one after the last
    /// (`" . . F ...  openSUSE_Tumbleweed x86_64 (published)"`, note the
    /// double space before the repo name), as opposed to the flush format
    /// the other tests here use. Both must parse to the same codes.
    #[test]
    fn reads_status_codes_in_the_padded_real_world_format() {
        let stdout = "\
python-foo
python-bar
python-baz
 . B U  openSUSE_Tumbleweed x86_64 (published)
Legend:
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), Some(&BuildStatus::Broken));
        assert_eq!(statuses.get("python-baz"), Some(&BuildStatus::Unresolvable));
    }

    #[test]
    fn a_space_code_is_treated_as_no_status_rather_than_disabled() {
        // The blank/disabled code is indistinguishable from the column
        // separator, so it must be skipped rather than recorded — a caller
        // treating "no entry" as "not resolvable" is the safe default.
        let stdout = "\
python-foo
python-bar
.   openSUSE_Tumbleweed x86_64 (published)
Legend:
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");

        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Succeeded));
        assert_eq!(statuses.get("python-bar"), None);
    }

    #[test]
    fn stops_parsing_at_the_legend_footer() {
        let stdout = "\
python-foo
F openSUSE_Tumbleweed x86_64 (published)
Legend:
F failed
";
        let statuses = parse_project_build_statuses(stdout, "openSUSE_Tumbleweed", "x86_64");
        assert_eq!(statuses.get("python-foo"), Some(&BuildStatus::Failed));
        assert_eq!(statuses.len(), 1);
    }
}

#[cfg(test)]
mod project_repo_paths_tests {
    use super::*;

    #[test]
    fn extracts_project_and_repository_from_each_path() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
    <arch>x86_64</arch>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }

    #[test]
    fn deduplicates_the_same_pair_across_repositories() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
  <repository name=\"openSUSE_Tumbleweed_i586\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }

    #[test]
    fn has_repository_true_when_one_is_present() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        assert!(parse_has_repository(xml));
    }

    #[test]
    fn has_repository_false_on_empty_project() {
        let xml = "\
<project name=\"home:dspinella:python\">
  <title>empty</title>
  <description/>
</project>
";
        assert!(!parse_has_repository(xml));
    }

    #[test]
    fn has_repository_false_when_only_devel_project_present() {
        // A `<devel project="..."/>` element is not a build repository and
        // must not be mistaken for one.
        let xml = "\
<project name=\"home:dspinella:python\">
  <devel project=\"devel:languages:python\"/>
</project>
";
        assert!(!parse_has_repository(xml));
    }

    #[test]
    fn a_devel_project_attribute_elsewhere_is_not_mistaken_for_a_path() {
        // `<devel project="..."/>` uses the same `project="..."` attribute
        // shape but is not a build path and has no `repository` attribute.
        let xml = "\
<project name=\"python-foo\">
  <devel project=\"devel:languages:python\"/>
  <repository name=\"openSUSE_Tumbleweed\">
    <path project=\"openSUSE:Factory\" repository=\"snapshot\"/>
  </repository>
</project>
";
        let paths = parse_project_repo_paths(xml);
        assert_eq!(
            paths,
            vec![("openSUSE:Factory".to_string(), "snapshot".to_string())]
        );
    }
}

#[cfg(test)]
mod rpmspec_sandbox_tests {
    use super::*;

    #[test]
    fn code_running_macros_are_recognised() {
        assert_eq!(code_macro_marker("Summary: %(id -u)"), Some("%("));
        assert_eq!(
            code_macro_marker("%global x %{lua: print(1)}"),
            Some("%{lua:")
        );
        assert_eq!(
            code_macro_marker("%description\nuse %{name} and %{python_module foo}"),
            None
        );
    }

    /// A spec whose `Summary` runs `touch` on expansion must not create the
    /// marker on the host. With bubblewrap the expansion runs in a sandbox
    /// where the marker's directory does not exist; without it the spec is
    /// refused before rpmspec sees it.
    #[test]
    fn spec_macro_cannot_touch_a_marker_on_the_host() {
        let outside = tempfile::tempdir().unwrap();
        let marker = outside.path().join("pwned");
        let work = tempfile::tempdir().unwrap();
        let spec = work.path().join("demo.spec");
        let text = format!(
            "Name: demo\nVersion: 1.0\nRelease: 0\nSummary: %(touch {}; echo x)\n\
             License: MIT\n%description\nx\n%files\n",
            marker.display()
        );
        fs::write(&spec, text).unwrap();

        let result = rpmspec_check(&spec, Emit::quiet());

        assert!(!marker.exists(), "spec macro ran on the host");
        if rpmspec_sandbox_works() {
            let out = result.expect("rpmspec should expand inside the sandbox");
            assert!(out.contains("Summary: x"), "{out}");
        }
    }

    #[test]
    fn refusal_names_the_macro_and_the_spec() {
        let err = Error::UnsandboxedMacro {
            spec: "/co/demo.spec".into(),
            marker: "%(".into(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/co/demo.spec") && msg.contains("`%(`"),
            "{msg}"
        );
    }
}

#[cfg(test)]
mod publish_plan_tests {
    use super::*;

    /// Backups, build directories and stray files stay out of the commit.
    /// The referenced patch goes in; the unreferenced one does not.
    #[test]
    fn only_the_package_files_are_published() {
        let dir = tempfile::tempdir().unwrap();
        let spec =
            "Name: pkg\nVersion: 1.0\nSource0: pkg-%{version}.tar.gz\nPatch0: 0001-fix.patch\n";
        for (name, body) in [
            ("pkg.spec", spec),
            ("pkg-1.0.tar.gz", ""),
            ("_service", ""),
            ("pkg.changes", ""),
            ("pkg.rpmlintrc", ""),
            ("0001-fix.patch", ""),
            ("extra.patch", ""),
            ("pkg.spec.preexisting", spec),
            ("pkg.changes.preexisting", ""),
            ("stray.txt", ""),
        ] {
            fs::write(dir.path().join(name), body).unwrap();
        }
        fs::create_dir(dir.path().join(".osc")).unwrap();
        fs::create_dir(dir.path().join("build")).unwrap();

        let plan = publish_plan(dir.path());

        assert_eq!(
            plan.files,
            [
                "0001-fix.patch",
                "_service",
                "pkg-1.0.tar.gz",
                "pkg.changes",
                "pkg.rpmlintrc",
                "pkg.spec",
            ]
        );
        assert_eq!(
            plan.skipped,
            [
                "build",
                "extra.patch",
                "pkg.changes.preexisting",
                "pkg.spec.preexisting",
                "stray.txt",
            ]
        );
    }

    /// A `Source:` the service fetched (the upstream `LICENSE`) is published,
    /// by its URL's last segment, its `#/` rename, or a macro pattern.
    #[test]
    fn a_file_a_source_line_names_is_published() {
        let dir = tempfile::tempdir().unwrap();
        let spec = "Name: pkg\n\
            Source0: https://example.org/pkg-%{version}.tar.gz\n\
            Source2: https://raw.githubusercontent.com/o/r/%{version}/LICENSE\n\
            Source3: https://example.org/dl?id=7#/pkg-data.json\n\
            Source4: data-%{version}.bin\n\
            # Source5: commented.txt\n";
        for (name, body) in [
            ("pkg.spec", spec),
            ("LICENSE", ""),
            ("pkg-data.json", ""),
            ("data-1.2.bin", ""),
            ("commented.txt", ""),
            ("LICENSE.bak", ""),
        ] {
            fs::write(dir.path().join(name), body).unwrap();
        }

        let plan = publish_plan(dir.path());

        assert_eq!(
            plan.files,
            ["LICENSE", "data-1.2.bin", "pkg-data.json", "pkg.spec"]
        );
        assert_eq!(plan.skipped, ["LICENSE.bak", "commented.txt"]);
    }

    /// `Version` is known, so another release's archive is not "named by the
    /// spec" just because the Source line says `%{version}`.
    #[test]
    fn the_spec_version_is_not_a_wildcard() {
        let names = spec_source_names(
            "Name: python-pkg\nVersion: 2.3.0\nSource: https://x/pkg-%{version}.tar.gz\n",
        );
        assert_eq!(names, ["pkg-2.3.0.tar.gz"]);
        assert!(
            !names
                .iter()
                .any(|p| matches_source_name(p, "pkg-1.27.2.tar.gz"))
        );
    }

    #[test]
    fn a_macro_in_a_source_name_matches_any_text() {
        assert!(matches_source_name("pkg-%{version}.dat", "pkg-1.0.dat"));
        assert!(matches_source_name("%{name}-%version.dat", "x-1.dat"));
        assert!(matches_source_name("LICENSE", "LICENSE"));
        assert!(!matches_source_name(
            "pkg-%{version}.dat",
            "pkg-1.0.dat.bak"
        ));
        assert!(!matches_source_name("pkg-%{version}.dat", "other-1.0.dat"));
        assert!(!matches_source_name("LICENSE", "LICENSE.bak"));
        assert!(!matches_source_name("a%{x}a", "a"));
    }

    #[test]
    fn a_patch_named_by_the_series_file_is_published() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pkg.spec"), "Name: pkg\n").unwrap();
        fs::write(dir.path().join("series"), "# quilt\n0002-quilt.patch\n").unwrap();
        fs::write(dir.path().join("0002-quilt.patch"), "").unwrap();

        let plan = publish_plan(dir.path());

        assert!(
            plan.files.contains(&"0002-quilt.patch".to_string()),
            "{plan:?}"
        );
    }

    #[test]
    fn nothing_is_committed_without_a_file_to_commit() {
        let dir = tempfile::tempdir().unwrap();
        let err = commit(dir.path(), "msg", &[], Emit::quiet()).unwrap_err();
        assert!(matches!(err, Error::NothingToPublish(_)), "{err}");
    }
}

#[cfg(test)]
mod service_validation_tests {
    use super::validate_service_xml;

    #[test]
    fn the_rendered_services_pass() {
        let xml = r#"<services>
  <service mode="manual" name="tar_scm">
    <param name="url">https://github.com/o/r.git</param>
    <param name="revision">v1.0</param>
    <param name="changesauthor"> YOUR EMAIL HERE </param>
  </service>
  <service mode="manual" name="set_version"/>
  <service name="cargo_vendor" mode="manual">
    <param name="src">r</param>
  </service>
</services>"#;
        validate_service_xml(xml.as_bytes()).unwrap();
    }

    #[test]
    fn common_tar_scm_and_cargo_vendor_params_pass() {
        let xml = r#"<services>
  <service name="tar_scm">
    <param name="url">https://github.com/o/r.git</param>
    <param name="exclude">.git</param>
    <param name="include">src</param>
    <param name="submodules">enable</param>
    <param name="depth">1</param>
  </service>
  <service name="cargo_vendor">
    <param name="src">r</param>
    <param name="versioned-dirs">true</param>
    <param name="respect-lockfile">true</param>
  </service>
</services>"#;
        validate_service_xml(xml.as_bytes()).unwrap();
        let bad = r#"<services><service name="tar_scm"><param name="extract">../../x</param></service></services>"#;
        assert!(validate_service_xml(bad.as_bytes()).is_err());
    }

    #[test]
    fn node_modules_params_pass_and_paths_are_confined() {
        let xml = r#"<services>
  <service name="node_modules" mode="manual">
    <param name="input">package-lock.json</param>
    <param name="node-dir">node_modules</param>
    <param name="omit-optional">true</param>
    <param name="legacy-container"/>
    <param name="cpio">node_modules.obscpio</param>
    <param name="output">node_modules.spec.inc</param>
    <param name="source-offset">10000</param>
  </service>
</services>"#;
        validate_service_xml(xml.as_bytes()).unwrap();
        for p in ["input", "node-dir", "cpio", "output"] {
            for v in ["../x", "/etc/x", "&#46;&#46;/x"] {
                let bad = format!(
                    r#"<services><service name="node_modules"><param name="{p}">{v}</param></service></services>"#
                );
                assert!(validate_service_xml(bad.as_bytes()).is_err(), "{bad}");
            }
        }
    }

    #[test]
    fn non_utf8_input_is_refused() {
        assert!(validate_service_xml(b"<services><!-- \xff --></services>").is_err());
    }

    #[test]
    fn decoded_values_are_what_is_checked() {
        let xml = r#"<services><service name="tar_scm"><param name="url">https://x/a&amp;b</param><param name="revision"><![CDATA[v1]]></param></service></services>"#;
        validate_service_xml(xml.as_bytes()).unwrap();
    }

    #[test]
    fn unknown_services_params_and_urls_are_refused() {
        for xml in [
            r#"<services><service name="evil"/></services>"#,
            r#"<services><service name="tar_scm"><param name="command">id</param></service></services>"#,
            r#"<services><service name="tar_scm"><param name="url">file:///etc</param></service></services>"#,
            r#"<services><service name="tar_scm"><param name="url">git://x/y</param></service></services>"#,
            r#"<services><service name="cargo_vendor"><param name="src">../x</param></service></services>"#,
            r#"<!DOCTYPE x [<!ENTITY a "b">]><services/>"#,
            r#"<services><service name="cargo_vendor"><param name="src">&#46;&#46;/x</param></service></services>"#,
            r#"<services><service name="tar_scm"><param name="revision">&#45;&#45;opt</param></service></services>"#,
            r#"<services><service name="tar_scm"><param name="url">&xxe;</param></service></services>"#,
            r#"<services><service name="tar_scm"><param name="url">https://x</service></services>"#,
            r#"<services><service name="tar_scm"><?pi x?></service></services>"#,
            r#"<services><service name="tar_scm"><evil/></service></services>"#,
        ] {
            assert!(validate_service_xml(xml.as_bytes()).is_err(), "{xml}");
        }
    }
}

#[cfg(test)]
mod dash_name_tests {
    use super::*;

    #[test]
    fn names_that_look_like_options_are_refused() {
        assert!(!is_valid_obs_name("--help"));
        assert!(!is_valid_obs_name("-x"));
        assert!(is_valid_obs_name("python-foo"));
        assert!(check_obs_name("package", "-x").is_err());
        assert!(fetch_project_packages("-x", Emit::quiet()).is_err());
        assert!(list_package_files("-x", "p", Emit::quiet()).is_none());
    }
}

#[cfg(test)]
mod repo_arch_tests {
    use super::*;

    const META: &str = r#"<project name="home:u:p">
  <repository name="openSUSE_Tumbleweed">
    <path project="openSUSE:Factory" repository="snapshot"/>
    <arch>x86_64</arch>
    <arch>aarch64</arch>
  </repository>
</project>"#;

    #[test]
    fn a_repository_and_arch_the_project_builds_are_accepted() {
        let info = parse_project_info(META);
        assert!(check_repo_arch(&info, "home:u:p", "openSUSE_Tumbleweed", "aarch64").is_ok());
    }

    #[test]
    fn a_misspelt_repository_or_missing_arch_names_what_exists() {
        let info = parse_project_info(META);
        let e = check_repo_arch(&info, "home:u:p", "Tumbleweed", "x86_64").unwrap_err();
        assert!(e.contains("openSUSE_Tumbleweed"), "{e}");
        let e = check_repo_arch(&info, "home:u:p", "openSUSE_Tumbleweed", "i586").unwrap_err();
        assert!(e.contains("x86_64, aarch64"), "{e}");
    }

    #[test]
    fn a_meta_without_repositories_is_not_judged_here() {
        let info = parse_project_info("<project name=\"p\"/>");
        assert!(check_repo_arch(&info, "p", "r", "a").is_ok());
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;

    #[test]
    fn the_delay_grows_while_the_status_holds_still() {
        let d = |n| poll_delay(n, 0, 0.5).as_secs();
        assert_eq!(
            (d(0), d(3), d(4), d(7), d(8), d(100)),
            (15, 15, 30, 30, 60, 60)
        );
    }

    #[test]
    fn failing_polls_back_off_to_two_minutes() {
        let d = |e| poll_delay(0, e, 0.5).as_secs();
        assert_eq!((d(1), d(2), d(3), d(8)), (30, 60, 120, 120));
        assert_eq!(poll_delay(8, 5, 0.5).as_secs(), 120);
    }

    #[test]
    fn jitter_stays_within_a_fifth() {
        assert_eq!(poll_delay(0, 0, 0.0).as_secs_f64(), 12.0);
        assert!(poll_delay(0, 0, 0.999).as_secs_f64() < 18.0);
    }

    #[test]
    fn the_http_status_is_read_from_either_osc_wording() {
        assert_eq!(http_status("HTTP Error 429: Too Many Requests"), Some(429));
        assert_eq!(
            http_status("x\n503 Server Error: Service Unavailable for url"),
            Some(503)
        );
        assert_eq!(http_status("401 Client Error: Unauthorized"), Some(401));
        assert_eq!(http_status("connection reset by peer"), None);
        assert_eq!(http_status("took 5000 Server Error"), None);
    }
}

#[cfg(test)]
mod overall_status_tests {
    use super::*;

    fn overall(xml: &str) -> BuildStatus {
        overall_status(&obs_status::parse_results(xml), "foo").0
    }

    #[test]
    fn every_repository_and_arch_must_be_built_or_out_of_scope() {
        let doc = |second: &str| {
            format!(
                r#"<resultlist>
  <result repository="r" arch="x86_64"><status package="foo" code="succeeded"/></result>
  <result repository="r" arch="i586"><status package="foo" code="{second}"/></result>
  <result repository="r" arch="aarch64"><status package="bar" code="failed"/></result>
</resultlist>"#
            )
        };
        assert_eq!(overall(&doc("excluded")), BuildStatus::Succeeded);
        assert_eq!(overall(&doc("disabled")), BuildStatus::Succeeded);
        assert_eq!(overall(&doc("failed")), BuildStatus::Failed);
        assert_eq!(overall(&doc("building")), BuildStatus::Building);
    }

    #[test]
    fn the_blocking_repository_and_arch_is_named_and_a_failure_outranks_a_wait() {
        let xml = r#"<resultlist>
  <result repository="r" arch="x86_64"><status package="foo" code="building"/></result>
  <result repository="r" arch="i586"><status package="foo" code="failed"/></result>
</resultlist>"#;
        assert_eq!(
            overall_status(&obs_status::parse_results(xml), "foo"),
            (BuildStatus::Failed, Some("r/i586".to_string()))
        );
        let xml = r#"<resultlist>
  <result repository="r" arch="x86_64"><status package="foo" code="succeeded"/></result>
  <result repository="r" arch="i586"><status package="foo" code="scheduled"/></result>
</resultlist>"#;
        assert_eq!(
            overall_status(&obs_status::parse_results(xml), "foo"),
            (BuildStatus::Scheduled, Some("r/i586".to_string()))
        );
    }

    #[test]
    fn an_unresolvable_flavor_keeps_the_package_out_of_the_built_set() {
        let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="python-mcp" code="succeeded"/>
    <status package="python-mcp:test" code="unresolvable"/>
    <status package="python-ok" code="succeeded"/>
    <status package="python-ok:test" code="succeeded"/>
  </result>
</resultlist>"#;
        let folded = fold_flavor_statuses(project_statuses_from_xml(xml, "r", "x86_64").unwrap());
        assert!(!folded["python-mcp"].is_success());
        assert!(folded["python-ok"].is_success());
        assert!(folded.contains_key("python-mcp:test"));
    }

    /// python-redis: tests failing in `:test` do not unbuild the package.
    #[test]
    fn a_failing_test_flavor_does_not_unbuild_its_package() {
        let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="python-redis" code="succeeded"/>
    <status package="python-redis:test" code="failed"/>
    <status package="python-foo" code="succeeded"/>
    <status package="python-foo:openmpi4" code="failed"/>
  </result>
</resultlist>"#;
        let folded = fold_flavor_statuses(project_statuses_from_xml(xml, "r", "x86_64").unwrap());
        assert!(folded["python-redis"].is_success());
        assert_eq!(folded["python-redis:test"], BuildStatus::Failed);
        // Any other flavor that fails still counts.
        assert!(!folded["python-foo"].is_success());
    }

    #[test]
    fn a_failed_multibuild_flavor_counts_against_its_package() {
        let xml = r#"<resultlist>
  <result repository="r" arch="x86_64">
    <status package="foo" code="succeeded"/>
    <status package="foo:py313" code="failed"/>
    <status package="foobar" code="failed"/>
  </result>
</resultlist>"#;
        assert_eq!(overall(xml), BuildStatus::Failed);
    }

    #[test]
    fn nothing_built_is_not_a_success() {
        let xml = r#"<resultlist><result repository="r" arch="a"><status package="foo" code="excluded"/></result></resultlist>"#;
        assert!(!overall(xml).is_success());
        assert!(!overall("<resultlist/>").is_success());
    }
}

#[cfg(test)]
mod superseded_rpm_tests {
    use super::*;

    #[test]
    fn the_newest_changes_entry_is_matched_by_its_message() {
        let sep = "-".repeat(67);
        let changes = format!(
            "{sep}\nMon Jan  1 00:00:00 UTC 2024 - A <a@b.c>\n\n- Update to 2.0\n\n{sep}\n\
             Mon Jan  1 00:00:00 UTC 2023 - A <a@b.c>\n\n- Update to 1.0\n"
        );
        assert!(newest_changes_entry_says(&changes, "Update to 2.0"));
        // An older entry does not count, nor does a different version.
        assert!(!newest_changes_entry_says(&changes, "Update to 1.0"));
        assert!(!newest_changes_entry_says(&changes, "Update to 2.0.1"));
        assert!(!newest_changes_entry_says("", "Update to 2.0"));
    }

    #[test]
    fn the_package_name_is_what_precedes_version_and_release() {
        assert_eq!(
            rpm_base_name("python313-foo-bar-1.0.2-2.1.noarch.rpm"),
            Some("python313-foo-bar")
        );
        assert_eq!(rpm_base_name("libfoo1-3-1.1.x86_64.rpm"), Some("libfoo1"));
        assert_eq!(rpm_base_name("notes.txt"), None);
        assert_eq!(rpm_base_name("x.rpm"), None);
    }

    #[test]
    fn every_file_of_a_listed_package_goes_and_everything_else_stays() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "python313-foo-1.0-1.1.noarch.rpm",
            "python313-foo-2.0-1.1.noarch.rpm",
            "python313-foobar-1.0-1.1.noarch.rpm",
            "other-3-1.1.x86_64.rpm",
            "python-foo-1.0-1.1.src.rpm",
        ] {
            fs::write(dir.path().join(f), "").unwrap();
        }
        remove_superseded_rpms(
            dir.path(),
            &["python313-foo-2.0-1.1.noarch.rpm".to_string()],
        );
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "other-3-1.1.x86_64.rpm",
                "python-foo-1.0-1.1.src.rpm",
                "python313-foobar-1.0-1.1.noarch.rpm",
            ]
        );
    }
}

#[cfg(test)]
mod redist_name_tests {
    use super::*;
    use crate::redist::TestMode;

    #[test]
    fn a_cuda_wheel_is_python_prefixed_unless_the_mode_is_on() {
        {
            let _off = TestMode::set(false);
            assert_eq!(rpm_name("nvidia-cublas"), "python-nvidia-cublas");
            assert_eq!(
                rpm_name("nvidia-cuda-runtime"),
                "python-nvidia-cuda-runtime"
            );
            assert_eq!(rpm_name("cuda-cublas"), "python-cuda-cublas");
        }
        let _on = TestMode::set(true);
        assert_eq!(rpm_name("nvidia-cublas"), "cuda-cublas");
        assert_eq!(rpm_name("nvidia_cublas_cu13"), "cuda-cublas");
        assert_eq!(rpm_name("nvidia-cuda-runtime"), "cuda-cudart");
        assert_eq!(rpm_name("nvidia-cudnn-cu13"), "cuda-cudnn");
        // Idempotent for the package's own name, and untouched for the rest.
        assert_eq!(rpm_name("cuda-cublas"), "cuda-cublas");
        assert_eq!(rpm_name("numpy"), "python-numpy");
        assert_eq!(rpm_name("nvidia-smi"), "python-nvidia-smi");
        // The node and the package it becomes agree on the comparison key.
        assert_eq!(
            fuzzy_key(&rpm_name("nvidia-cublas")),
            fuzzy_key("cuda-cublas")
        );
    }
}

#[cfg(test)]
mod transient_broken_tests {
    use super::*;

    #[test]
    fn broken_needs_a_nonempty_log_to_be_confirmed() {
        let dir = std::env::temp_dir().join(format!("ap-broken-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("build.log");
        assert!(!broken_is_confirmed(&log), "missing log");
        fs::write(&log, "\n  \n").unwrap();
        assert!(!broken_is_confirmed(&log), "empty log");
        fs::write(&log, "[1s] error: bad source\n").unwrap();
        assert!(broken_is_confirmed(&log));
        fs::remove_dir_all(&dir).ok();
    }

    fn emit_quiet() -> Emit<'static> {
        Emit::quiet()
    }

    #[test]
    fn an_unconfirmed_broken_is_re_triggered_until_it_builds() {
        let mut builds = 0;
        let mut rebuilds = 0;
        let r = build_with_retriggers(
            "python-x",
            emit_quiet(),
            Duration::ZERO,
            || {
                builds += 1;
                if builds < 3 {
                    Err(Error::NotActionable(BROKEN_UNCONFIRMED.to_string()))
                } else {
                    Ok(())
                }
            },
            || {
                rebuilds += 1;
                Ok(())
            },
        );
        assert!(r.is_ok());
        assert_eq!((builds, rebuilds), (3, 2));
    }

    #[test]
    fn an_unconfirmed_broken_that_persists_stands_as_broken_after_the_bound() {
        let mut builds = 0;
        let r = build_with_retriggers(
            "python-x",
            emit_quiet(),
            Duration::ZERO,
            || {
                builds += 1;
                Err(Error::NotActionable(BROKEN_UNCONFIRMED.to_string()))
            },
            // A failing re-trigger only warns; the loop still ends.
            || Err(Error::NotActionable("rebuild refused".into())),
        );
        assert_eq!(builds, MAX_BROKEN_RETRIGGERS + 1);
        match r {
            Err(Error::NotActionable(m)) => assert_eq!(m, BROKEN_MESSAGE),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn any_other_result_is_returned_without_a_retrigger() {
        for (expect_ok, result) in [
            (
                false,
                Err(Error::NotActionable("OBS: build is disabled".into())),
            ),
            (false, Err(Error::NotActionable(BROKEN_MESSAGE.into()))),
            (true, Ok(())),
        ] {
            let mut rebuilds = 0;
            let mut result = Some(result);
            let r = build_with_retriggers(
                "python-x",
                emit_quiet(),
                Duration::ZERO,
                || result.take().unwrap(),
                || {
                    rebuilds += 1;
                    Ok(())
                },
            );
            assert_eq!(rebuilds, 0);
            assert_eq!(r.is_ok(), expect_ok);
        }
    }

    #[test]
    fn getbinaries_is_retried_and_a_late_success_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut calls = 0;
        let r = fetch_with_retries(
            "p",
            "python-x",
            "r",
            "x86_64",
            dir.path(),
            emit_quiet(),
            4,
            Duration::ZERO,
            || {
                calls += 1;
                if calls < 3 {
                    Err(Error::OscFailed {
                        cmd: "getbinaries".into(),
                        code: 1,
                        stderr: "502".into(),
                    })
                } else {
                    Ok(())
                }
            },
        );
        assert!(r.is_ok());
        assert_eq!(calls, 3);
    }

    #[test]
    fn getbinaries_gives_up_after_its_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let mut calls = 0;
        let r = fetch_with_retries(
            "p",
            "python-x",
            "r",
            "x86_64",
            dir.path(),
            emit_quiet(),
            4,
            Duration::ZERO,
            || {
                calls += 1;
                Err(Error::OscFailed {
                    cmd: "getbinaries".into(),
                    code: 1,
                    stderr: "502".into(),
                })
            },
        );
        assert!(matches!(r, Err(Error::OscFailed { code: 1, .. })));
        assert_eq!(calls, 4);
    }

    #[test]
    fn retry_stops_at_the_first_success() {
        let mut calls = 0;
        let r: Result<u32, &str> = retry(4, Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Err("transient")
            } else {
                Ok(calls)
            }
        });
        assert_eq!(r, Ok(3));
    }

    #[test]
    fn retry_gives_up_after_the_bound() {
        let mut calls = 0;
        let r: Result<(), &str> = retry(4, Duration::ZERO, || {
            calls += 1;
            Err("down")
        });
        assert_eq!(r, Err("down"));
        assert_eq!(calls, 4);
    }
}

#[cfg(test)]
mod transient_error_tests {
    use super::*;

    fn failed(code: i32, stderr: &str) -> Error {
        Error::OscFailed {
            cmd: "x".into(),
            code,
            stderr: stderr.into(),
        }
    }

    #[test]
    fn network_obs_and_disk_failures_are_transient_and_package_failures_are_not() {
        assert!(failed(1, "HTTP Error 503: Service Unavailable").is_transient());
        assert!(failed(1, "502 Server Error: Bad Gateway for url").is_transient());
        assert!(
            failed(
                1,
                "fatal: unable to access: Could not resolve host: github.com"
            )
            .is_transient()
        );
        assert!(failed(28, "").is_transient());
        assert!(
            failed(
                -1,
                "gave up after 8 consecutive failed status polls; last error: x"
            )
            .is_transient()
        );
        assert!(failed(-1, "timed out after 7200s waiting for OBS build").is_transient());
        assert!(
            Error::Process(proc::ProcessError::Timeout {
                program: "osc".into(),
                secs: 1
            })
            .is_transient()
        );
        assert!(Error::Io(std::io::Error::from(std::io::ErrorKind::TimedOut)).is_transient());
        assert!(
            Error::NotActionable(format!(
                "OBS reported p {DOWNLOAD_FAILED}: osc getbinaries failed"
            ))
            .is_transient()
        );
        // The package is at fault, or the run was stopped.
        assert!(!failed(1, "HTTP Error 404: Not Found").is_transient());
        assert!(!failed(1, "HTTP Error 403: Forbidden").is_transient());
        assert!(!failed(-1, "interrupted").is_transient());
        assert!(!Error::NotActionable(BROKEN_MESSAGE.into()).is_transient());
        assert!(
            !Error::UnresolvableDeps {
                names: vec![],
                detail: vec![]
            }
            .is_transient()
        );
        assert!(!Error::Io(std::io::Error::from(std::io::ErrorKind::NotFound)).is_transient());
    }

    #[test]
    fn transient_text_is_recognised() {
        assert!(text_is_transient("Connection reset by peer"));
        assert!(text_is_transient("No space left on device"));
        assert!(!text_is_transient("version 2.0.0 is yanked"));
        // A word of its own, not a name that contains it.
        assert!(text_is_transient("Error: Timeout"));
        assert!(text_is_transient("operation timed out"));
        assert!(!text_is_transient(
            "FAILED tests/test_net.py::test_timeout_is_set"
        ));
        assert!(!text_is_transient("unknown option --timeouts"));
    }
}
