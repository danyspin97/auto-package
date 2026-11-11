//! The `doctor` sub-command: the environment checks of `envcheck`, for the
//! kinds of work named, and what to install or set up for each missing one.
//!
//! It makes no network request of its own: it runs local programs (`osc`,
//! `rpmspec`, `claude auth status`, `sudo -n -l osc build` and the like). Only
//! `--online` adds a request, `osc whois`. It never installs anything: it
//! prints the command. No project, work directory, lock, agent login or
//! interrupt handler is involved, so it runs anywhere the binary does.

use std::collections::BTreeSet;
use std::io::{IsTerminal, Write};

use ap_build::envcheck::{self, CheckResult, Needs, Outcome, Probe, Required};
use ap_core::remedy::{Cause, HostDistro, parse_os_release, printable};
use ap_core::sink::{Role, pad_to, paint, stdout_colour, terminal_width};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

#[derive(Args)]
pub(super) struct DoctorArgs {
    /// The kinds of work to check for: rust, python, go, npm, local or all.
    /// Repeatable, or a comma-separated list. Without it, all (go and npm
    /// then only as optional rows).
    #[arg(long = "for", value_enum, value_delimiter = ',', value_name = "KIND")]
    targets: Vec<Target>,

    /// Skip the claude login check: the run makes no model call.
    #[arg(long)]
    pub(super) no_agent: bool,

    /// Also run `osc whois`, which asks OBS who you are with your credentials
    /// (a network request). Off by default; "offline" means no OBS request.
    #[arg(long)]
    pub(super) online: bool,

    /// Print the results as JSON on stdout (never coloured).
    #[arg(long)]
    pub(super) json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum Target {
    Rust,
    Python,
    Go,
    Npm,
    Local,
    All,
}

/// Whether a run of `args` checks for `target`: with no `--for`, or with
/// `all`, every kind.
fn wants(args: &DoctorArgs, target: Target) -> bool {
    args.targets.is_empty() || args.targets.contains(&Target::All) || args.targets.contains(&target)
}

/// The rows a run of `args` needs: the kinds named, the agent unless
/// `--no-agent`, and the network only with `--online`. The OBS services of
/// go and npm are required only when named; see [`optional_services`].
pub(super) fn needs_for(args: &DoctorArgs) -> Needs {
    let mut services = BTreeSet::new();
    if wants(args, Target::Rust) {
        services.insert("cargo_vendor");
    }
    if args.targets.contains(&Target::Go) {
        services.insert("go_modules");
    }
    if args.targets.contains(&Target::Npm) {
        services.insert("node_modules");
    }
    Needs {
        agent: !args.no_agent,
        local: wants(args, Target::Local),
        services: services.into_iter().map(str::to_string).collect(),
        rust: wants(args, Target::Rust),
        online: args.online,
    }
}

/// The OBS services a run uses for go or npm packages without naming them:
/// under the default kinds, their rows are optional.
pub(super) fn optional_services(args: &DoctorArgs) -> Vec<&'static str> {
    let mut names = Vec::new();
    if wants(args, Target::Go) && !args.targets.contains(&Target::Go) {
        names.push("go_modules");
    }
    if wants(args, Target::Npm) && !args.targets.contains(&Target::Npm) {
        names.push("node_modules");
    }
    names
}

/// The checked rows for `args` on `probe`.
pub(super) fn check(args: &DoctorArgs, probe: &Probe<'_>) -> Vec<CheckResult> {
    let mut rows = envcheck::run(&needs_for(args), probe);
    let optional = optional_services(args);
    if !optional.is_empty() {
        rows.extend(envcheck::optional_service_rows(
            probe,
            &optional,
            "only needed for Go and npm packages",
        ));
    }
    rows
}

/// The first line of the report: the mode, and the kinds when `--for` named
/// some.
pub(super) fn heading(args: &DoctorArgs) -> String {
    let mode = if args.online { "online" } else { "offline" };
    let mut kinds: Vec<String> = Vec::new();
    for target in &args.targets {
        if let Some(value) = target.to_possible_value() {
            let name = value.get_name().to_string();
            if !kinds.contains(&name) {
                kinds.push(name);
            }
        }
    }
    if kinds.is_empty() {
        format!("{HEADER} ({mode})")
    } else {
        format!("{HEADER} ({mode}, for {})", kinds.join(", "))
    }
}

/// Exit status: 0 when no required row is missing (warnings never fail), 1
/// otherwise.
pub(super) fn exit_code(results: &[CheckResult]) -> i32 {
    if ready(results) { 0 } else { 1 }
}

/// Whether every required row is present.
fn ready(results: &[CheckResult]) -> bool {
    missing_required(results) == 0
}

fn missing_required(results: &[CheckResult]) -> usize {
    results
        .iter()
        .filter(|r| r.required == Required::Yes && status(&r.outcome) == Some(Status::Missing))
        .count()
}

/// The required rows whose probe could not run: their fact is not known.
fn unchecked_required(results: &[CheckResult]) -> usize {
    results
        .iter()
        .filter(|r| r.required == Required::Yes && matches!(r.outcome, Outcome::Unchecked(_)))
        .count()
}

/// The state of a row. `NotApplicable` rows are dropped by `envcheck::run`,
/// so they have no status here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Ok,
    Missing,
    Warn,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Missing => "missing",
            Status::Warn => "warn",
        }
    }
}

fn status(outcome: &Outcome) -> Option<Status> {
    match outcome {
        Outcome::Ok(_) => Some(Status::Ok),
        Outcome::Missing(_) | Outcome::MissingText(_) => Some(Status::Missing),
        Outcome::Warn(_) | Outcome::WarnText(_) | Outcome::Unchecked(_) => Some(Status::Warn),
        Outcome::NotApplicable => None,
    }
}

/// The text that says what was found, or what is wrong.
fn detail(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok(text)
        | Outcome::MissingText(text)
        | Outcome::WarnText(text)
        | Outcome::Unchecked(text) => printable(text),
        Outcome::Missing(cause) | Outcome::Warn(cause) => cause_text(cause),
        Outcome::NotApplicable => String::new(),
    }
}

fn cause_text(cause: &Cause) -> String {
    match cause {
        Cause::MissingTool {
            tool,
            path: Some(path),
        } => printable(&format!(
            "{tool} is not installed ({} is missing)",
            path.display()
        )),
        Cause::MissingTool { tool, path: None } => printable(&format!("{tool} is not installed")),
        Cause::ToolchainUnset { tool } => printable(&format!("no toolchain is set up for {tool}")),
        Cause::SandboxUnavailable { why } => printable(why),
    }
}

fn glyph(status: Status) -> (&'static str, Role) {
    match status {
        Status::Ok => ("✓", Role::Success),
        Status::Missing => ("✗", Role::Failure),
        Status::Warn => ("⚠", Role::Warning),
    }
}

/// A hint that is a command is shown in the command colour; prose is not.
fn is_command(hint: &str) -> bool {
    ["sudo ", "zypper ", "rustup "]
        .iter()
        .any(|prefix| hint.starts_with(prefix))
}

const HEADER: &str = "auto-package doctor: environment check";
const INDENT: usize = 2;
const MARKER_WIDTH: usize = 8;
const HINT_INDENT: usize = 6;
const DETAIL_INDENT: usize = 4;
/// The detail column needs at least this many columns to be read beside the
/// name; a narrower terminal gets the stacked layout.
const MIN_DETAIL_COLUMNS: usize = 30;

/// `text` split into lines of at most `avail` columns; a word wider than a
/// line gets a line to itself.
fn wrap(text: &str, avail: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if UnicodeWidthStr::width(line.as_str()) + 1 + UnicodeWidthStr::width(word) <= avail
        {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    lines.push(line);
    lines
}

/// The plain-text report, coloured when `colour` is set. `width` is the
/// number of columns the report lays out to: a row puts its detail beside its
/// name when that leaves [`MIN_DETAIL_COLUMNS`], and otherwise stacks the
/// detail under the name. `suse` says whether the host is openSUSE, whose
/// package names the hints use.
pub(super) fn render_text(
    results: &[CheckResult],
    heading: &str,
    colour: bool,
    width: usize,
    suse: bool,
) -> String {
    let rows: Vec<(&CheckResult, Status)> = results
        .iter()
        .filter_map(|r| status(&r.outcome).map(|s| (r, s)))
        .collect();
    let name_width = rows
        .iter()
        .map(|(r, _)| UnicodeWidthStr::width(r.name.as_str()))
        .max()
        .unwrap_or(0);
    // Indent, glyph, a space, the name, two spaces, the marker, two spaces.
    let detail_col = INDENT + 1 + 1 + name_width + 2 + MARKER_WIDTH + 2;
    let aligned = width >= detail_col + MIN_DETAIL_COLUMNS;
    let avail = if aligned {
        width - detail_col
    } else {
        width.saturating_sub(DETAIL_INDENT).max(1)
    };

    let mut out = format!("{}\n\n", paint(heading, Role::Heading, colour));
    for (r, s) in &rows {
        let (glyph, role) = glyph(*s);
        let marker = match r.required {
            Required::Yes => "required",
            Required::No => "optional",
        };
        let lines = wrap(&detail(&r.outcome), avail);
        if aligned {
            for (i, line) in lines.iter().enumerate() {
                if i == 0 {
                    out.push_str(&format!(
                        "{}{} {}  {}  {}\n",
                        " ".repeat(INDENT),
                        paint(glyph, role, colour),
                        pad_to(&r.name, name_width),
                        paint(&pad_to(marker, MARKER_WIDTH), Role::Dim, colour),
                        paint(line, Role::Dim, colour),
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}\n",
                        " ".repeat(detail_col),
                        paint(line, Role::Dim, colour)
                    ));
                }
            }
        } else {
            out.push_str(&format!(
                "{}{} {}  {}\n",
                " ".repeat(INDENT),
                paint(glyph, role, colour),
                r.name,
                paint(marker, Role::Dim, colour),
            ));
            for line in &lines {
                out.push_str(&format!(
                    "{}{}\n",
                    " ".repeat(DETAIL_INDENT),
                    paint(line, Role::Dim, colour)
                ));
            }
        }
        if let Some(hint) = &r.hint {
            let shown = if is_command(hint) {
                paint(hint, Role::Success, colour)
            } else {
                hint.clone()
            };
            out.push_str(&format!("{}{shown}\n", " ".repeat(HINT_INDENT)));
        }
    }

    out.push('\n');
    if let Some(line) = envcheck::zypper_line(results) {
        out.push_str("Install everything missing with:\n");
        out.push_str(&paint(&line, Role::Success, colour));
        out.push_str("\n\n");
    }
    if !suse && rows.iter().any(|(_, s)| *s != Status::Ok) {
        out.push_str("Package names are openSUSE's; on this host, look up the equivalents.\n\n");
    }
    let verdict = match (missing_required(results), unchecked_required(results)) {
        (0, 0) => paint("Ready.", Role::Success, colour),
        (0, n) => paint(
            &format!("Ready, but {n} required check(s) could not be run."),
            Role::Warning,
            colour,
        ),
        (n, _) => paint(
            &format!("{n} required item(s) missing."),
            Role::Failure,
            colour,
        ),
    };
    out.push_str(&verdict);
    out.push('\n');
    out
}

/// One row of the JSON report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct JsonCheck {
    pub id: String,
    pub name: String,
    pub required: bool,
    pub status: String,
    pub detail: String,
    pub hint: Option<String>,
    pub package: Option<String>,
}

/// The JSON report: the rows, the zypper line and the verdict. `unchecked`
/// counts the required rows whose probe could not run (`ready` stays true
/// for them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct JsonReport {
    pub checks: Vec<JsonCheck>,
    pub zypper: Option<String>,
    pub ready: bool,
    pub unchecked: usize,
}

pub(super) fn json_report(results: &[CheckResult]) -> JsonReport {
    let checks = results
        .iter()
        .filter_map(|r| {
            let s = status(&r.outcome)?;
            Some(JsonCheck {
                id: r.id.clone(),
                name: r.name.clone(),
                required: r.required == Required::Yes,
                status: s.word().to_string(),
                detail: detail(&r.outcome),
                hint: r.hint.clone(),
                package: r.package.map(str::to_string),
            })
        })
        .collect();
    JsonReport {
        checks,
        zypper: envcheck::zypper_line(results),
        ready: ready(results),
        unchecked: unchecked_required(results),
    }
}

/// The JSON report as text, never coloured.
pub(super) fn render_json(results: &[CheckResult]) -> String {
    let mut text = serde_json::to_string_pretty(&json_report(results))
        .expect("a report of strings serializes");
    text.push('\n');
    text
}

/// Run the checks for `args` on this machine, print them and return the exit
/// status.
pub(super) fn main(args: &DoctorArgs) -> i32 {
    let probe = envcheck::host_probe();
    let results = check(args, &probe);
    let text = if args.json {
        render_json(&results)
    } else {
        let suse = parse_os_release(&probe.os_release) == HostDistro::Suse;
        let width = terminal_width(std::io::stdout().is_terminal());
        render_text(&results, &heading(args), stdout_colour(), width, suse)
    };
    // A closed pipe is not an error of the check.
    let _ = std::io::stdout().write_all(text.as_bytes());
    exit_code(&results)
}
