//! `auto-package explain <pkg>`: what happened to one package in a work
//! directory, read back from the files a run leaves behind. No model, no
//! network, no writes: the report is a deterministic function of
//! `state/<pkg>/`, `runs/<run>/run.jsonl`, `runs/<run>/<pkg>/NNN-<step>/`, the
//! package notes and the checkout.
//!
//! [`explain`] collects an [`Explanation`] (serialisable: `--json`), and
//! [`render`] turns it into styled lines for a given width, which the command
//! prints with ANSI colours and the TUI shows in a modal.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// The agent budget per package when the run's command line does not give one
/// (the default of `--budget-usd`).
pub const DEFAULT_BUDGET_USD: f64 = 10.0;

const REASONING_MAX: usize = 700;
const EVIDENCE_MAX: usize = 260;
const EVIDENCE_LINES: usize = 5;
const LOG_ERROR_LINES: usize = 8;
const NOTES_LINES: usize = 60;

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Explanation {
    pub package: String,
    pub project_dir: String,
    /// The run the report is about (`None`: no run touched the package).
    pub run: Option<String>,
    /// Other runs that touched the package, newest first.
    pub other_runs: Vec<String>,
    pub run_info: RunInfo,
    pub version: VersionInfo,
    pub outcome: Outcome,
    pub attempts: Vec<Attempt>,
    pub flaky: Vec<String>,
    pub ladder: Ladder,
    pub cost: Cost,
    pub warnings: Vec<WarningLine>,
    pub skipped: Vec<String>,
    pub notes: Notes,
    /// Things the report cannot tell, said plainly.
    pub unknown: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct RunInfo {
    pub command: Option<String>,
    /// `Success`, `Failed`, ...; `None` when the run wrote no `run_finished`.
    pub finished: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub unreadable_lines: usize,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct VersionInfo {
    pub from: Option<String>,
    pub to: Option<String>,
    pub reason: Option<String>,
    pub reason_raw: Option<String>,
    pub blocked_by: Vec<String>,
    /// How many times the run selected the package (more than once: it came
    /// back after its dependencies were built).
    pub selections: usize,
    /// `--revision` / `--to-version` from the run's command line.
    pub revision: Option<String>,
    pub forced_version: Option<String>,
    /// `Version:` of the spec in the checkout now.
    pub spec_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Outcome {
    /// `built`, `already built`, `failed`, `blocked`, `missing dependencies`
    /// or `no result recorded`.
    pub result: String,
    pub detail: Option<String>,
    /// `last_outcome` and `last_failure_reason` of state.json (the latest
    /// run's view, not necessarily this run's).
    pub state_outcome: Option<String>,
    pub state_reason: Option<String>,
    pub log_source: Option<String>,
    pub log_errors: Vec<String>,
    /// The newest changelog entry of the checkout.
    pub changelog: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Attempt {
    pub step: u64,
    pub kind: String,
    pub attempt: Option<u64>,
    pub round: Option<u64>,
    pub detail: Option<String>,
    pub outcome: Option<String>,
    pub elapsed_ms: Option<u64>,
    /// Ordinal among the agent turns of the run for this package.
    pub agent_turn: Option<usize>,
    pub agent: Option<Agent>,
    pub spec_added: Option<u64>,
    pub spec_removed: Option<u64>,
    pub rejected: Option<String>,
    pub build: Option<BuildInfo>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Agent {
    pub task: Option<String>,
    pub action: Option<String>,
    pub reasoning: Option<String>,
    pub evidence: Vec<String>,
    pub confidence: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct BuildInfo {
    pub classification: Option<String>,
    pub missing: Vec<String>,
    pub extracts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Ladder {
    pub max_round: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Cost {
    /// Sum of the agent turns and package-notes turns of this run's events.
    pub total_usd: f64,
    pub turns: usize,
    pub budget_usd: f64,
    /// Whether the budget is the default (the command line did not say).
    pub budget_is_default: bool,
    /// state.json's spend, when it belongs to this run.
    pub state_spent_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct WarningLine {
    pub step: Option<u64>,
    pub message: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct Notes {
    /// Headings of state/<pkg>/notes.md (one per attempt it records).
    pub run_note_headings: Vec<String>,
    /// <notes dir>/<pkg>.md: what the tool keeps across runs.
    pub package_notes: String,
}

/// What to explain.
pub struct Query<'a> {
    /// The per-project directory holding `state/`, `runs/` and the checkout.
    pub project_dir: &'a Path,
    pub package: &'a str,
    /// A run id; `None` for the latest run that touched the package.
    pub run: Option<&'a str>,
    /// The text of `<notes dir>/<pkg>.md` (empty when there is none).
    pub package_notes: String,
}

/// The per-project directory for `--work-dir` / `--project` / `--arch`: the
/// work directory itself when it already holds `runs` or `state`, the
/// `<project>-<arch>` subdirectory when a project is given, and otherwise the
/// only subdirectory that knows `package`.
pub fn resolve_project_dir(
    work_dir: &Path,
    project: Option<&str>,
    arch: &str,
    package: &str,
) -> Result<PathBuf, String> {
    if work_dir.join("runs").is_dir() || work_dir.join("state").is_dir() {
        return Ok(work_dir.to_path_buf());
    }
    if let Some(p) = project {
        let dir = work_dir.join(format!(
            "{}-{}",
            ap_core::run::sanitize_name(p),
            ap_core::run::sanitize_name(arch)
        ));
        return if dir.is_dir() {
            Ok(dir)
        } else {
            Err(format!(
                "no work directory for {p} ({arch}) at {}",
                dir.display()
            ))
        };
    }
    let mut known: Vec<PathBuf> = std::fs::read_dir(work_dir)
        .map_err(|e| format!("cannot read {}: {e}", work_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|d| d.join("state").join(package).is_dir())
        .collect();
    known.sort();
    match known.len() {
        1 => Ok(known.remove(0)),
        0 => Err(format!(
            "{} has no state for {package}; pass --project",
            work_dir.display()
        )),
        _ => Err(format!(
            "{package} is in several projects of {}; pass --project (one of: {})",
            work_dir.display(),
            known
                .iter()
                .filter_map(|d| d.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Collect what the work directory says about `q.package`.
pub fn explain(q: &Query) -> Result<Explanation, String> {
    let pkg = q.package;
    if pkg.is_empty() || pkg.contains('/') || pkg.starts_with('.') {
        return Err(format!("{pkg:?} is not a valid package name"));
    }
    let dir = q.project_dir;
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let mut ex = Explanation {
        package: pkg.to_string(),
        project_dir: dir.display().to_string(),
        ..Default::default()
    };
    let state = read_json(&dir.join("state").join(pkg).join("state.json"));
    let runs = runs_touching(dir, pkg);
    let chosen = match q.run {
        Some(r) => {
            if !dir.join("runs").join(r).is_dir() {
                return Err(format!(
                    "no run {r} in {} (runs: {})",
                    dir.display(),
                    list_runs(dir).join(", ")
                ));
            }
            Some(r.to_string())
        }
        None => runs.first().cloned(),
    };
    ex.other_runs = runs
        .iter()
        .filter(|r| Some(*r) != chosen.as_ref())
        .cloned()
        .collect();
    ex.run = chosen.clone();
    let obs_name = state
        .as_ref()
        .and_then(|s| s["obs_name"].as_str())
        .unwrap_or(pkg)
        .to_string();
    if state.is_none() && chosen.is_none() {
        return Err(format!(
            "nothing about {pkg} in {} (no state/{pkg} and no run touched it)",
            dir.display()
        ));
    }
    let run_pkg_dir = chosen.as_ref().map(|r| dir.join("runs").join(r).join(pkg));
    let mut p = Parsed::default();
    if let Some(r) = &chosen {
        p = parse_run(&dir.join("runs").join(r).join("run.jsonl"), pkg);
    }
    ex.run_info = RunInfo {
        command: p.argv.as_ref().map(|a| command_line(a)),
        finished: p.run_finished.as_ref().map(|v| label(&v["outcome"])),
        elapsed_ms: p.run_finished.as_ref().and_then(|v| v["elapsed"].as_u64()),
        unreadable_lines: p.bad_lines,
    };
    // Attach the artifact directories of the attempts.
    for a in &mut p.attempts {
        if let Some(d) = run_pkg_dir.as_deref().and_then(|d| step_dir(d, a.step)) {
            a.dir = Some(d.display().to_string());
        }
    }
    ex.version = version_info(&p, dir, &obs_name);
    ex.outcome = outcome(
        &p,
        state.as_ref(),
        dir,
        &obs_name,
        pkg,
        run_pkg_dir.as_deref(),
    );
    ex.flaky = flaky_lines(&p, &q.package_notes, dir, pkg, chosen.as_deref());
    ex.ladder = Ladder {
        max_round: p.attempts.iter().filter_map(|a| a.round).max(),
        reason: state
            .as_ref()
            .and_then(|s| s["ladder_reason"].as_str())
            .map(str::to_string),
    };
    let (budget, is_default) = budget(p.argv.as_deref());
    let turn_cost: f64 = p
        .attempts
        .iter()
        .filter_map(|a| a.agent.as_ref().and_then(|g| g.cost_usd))
        .sum();
    ex.cost = Cost {
        total_usd: turn_cost + p.notes_cost,
        turns: p.attempts.iter().filter(|a| a.agent.is_some()).count(),
        budget_usd: budget,
        budget_is_default: is_default,
        state_spent_usd: state.as_ref().and_then(|s| {
            (s["cumulative_spent_run_id"].as_str() == chosen.as_deref())
                .then(|| s["cumulative_spent_usd"].as_f64())
                .flatten()
        }),
    };
    ex.warnings = warnings(&p);
    ex.skipped = p.skipped.clone();
    let run_notes =
        std::fs::read_to_string(dir.join("state").join(pkg).join("notes.md")).unwrap_or_default();
    ex.notes = Notes {
        run_note_headings: run_notes
            .lines()
            .filter(|l| l.starts_with("## "))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .collect(),
        package_notes: q.package_notes.clone(),
    };
    ex.attempts = p.attempts;
    ex.unknown = unknowns(&ex, state.is_some());
    Ok(ex)
}

#[derive(Default)]
struct Parsed {
    argv: Option<Vec<String>>,
    run_finished: Option<Value>,
    selected: Vec<Value>,
    attempts: Vec<Attempt>,
    skipped: Vec<String>,
    warnings: Vec<(Option<u64>, String)>,
    rejected: Vec<(Option<u64>, String)>,
    finished: Option<Value>,
    build_log: Option<String>,
    notes_cost: f64,
    bad_lines: usize,
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn list_runs(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join("runs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Runs that touched the package, newest first (run ids start with the time).
fn runs_touching(dir: &Path, pkg: &str) -> Vec<String> {
    let needle = format!("\"pkg\":\"{pkg}\"");
    let mut out: Vec<String> = list_runs(dir)
        .into_iter()
        .filter(|r| {
            let run = dir.join("runs").join(r);
            run.join(pkg).is_dir()
                || std::fs::read_to_string(run.join("run.jsonl")).is_ok_and(|t| t.contains(&needle))
        })
        .collect();
    out.reverse();
    out
}

fn label(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) if m.len() == 1 => m.keys().next().cloned().unwrap_or_default(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn parse_run(path: &Path, pkg: &str) -> Parsed {
    let mut p = Parsed::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return p;
    };
    let needle = format!("\"pkg\":\"{pkg}\"");
    let mut agent_turns = 0;
    for line in text.lines() {
        let run_level = line.starts_with("{\"event\":\"run_started\"")
            || line.starts_with("{\"event\":\"run_finished\"");
        if !run_level && !line.contains(&needle) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            p.bad_lines += 1;
            continue;
        };
        if !run_level && v["pkg"].as_str() != Some(pkg) {
            continue;
        }
        let step = v["step"].as_u64();
        let attempt =
            |p: &mut Parsed| step.and_then(|s| p.attempts.iter().rposition(|a| a.step == s));
        match v["event"].as_str().unwrap_or("") {
            "run_started" => {
                p.argv = v["argv"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                });
            }
            "run_finished" => p.run_finished = Some(v),
            "target_selected" => p.selected.push(v),
            "step_started" => p.attempts.push(Attempt {
                step: step.unwrap_or(0),
                kind: label(&v["kind"]),
                attempt: v["attempt"].as_u64(),
                round: v["round"].as_u64(),
                detail: v["detail"].as_str().map(str::to_string),
                ..Default::default()
            }),
            "step_finished" => {
                if let Some(i) = attempt(&mut p) {
                    p.attempts[i].outcome = Some(label(&v["outcome"]));
                    p.attempts[i].elapsed_ms = v["elapsed"].as_u64();
                }
            }
            "step_skipped" => {
                let reason = match &v["reason"] {
                    Value::Object(m) => m.values().next().map(|x| label(x)).unwrap_or_default(),
                    other => label(other),
                };
                if label(&v["kind"]) == "AgentTurn" {
                    p.skipped.push(format!("agent turn skipped: {reason}"));
                }
            }
            "agent_prompt" => {
                if let Some(i) = attempt(&mut p) {
                    agent_turns += 1;
                    p.attempts[i].agent_turn = Some(agent_turns);
                    let task = match &v["task"] {
                        Value::Object(m) => m
                            .iter()
                            .next()
                            .map(|(k, x)| {
                                match (x["attempt"].as_u64(), x["max_attempts"].as_u64()) {
                                    (Some(a), Some(m)) => format!("{k} {a}/{m}"),
                                    _ => k.clone(),
                                }
                            })
                            .unwrap_or_default(),
                        other => label(other),
                    };
                    p.attempts[i].agent.get_or_insert_with(Agent::default).task = Some(task);
                }
            }
            "agent_reply" => {
                if let Some(i) = attempt(&mut p) {
                    let r = &v["response"];
                    let g = p.attempts[i].agent.get_or_insert_with(Agent::default);
                    g.action = r["action"].as_str().map(str::to_string);
                    g.reasoning = r["reasoning"].as_str().map(str::to_string);
                    g.confidence = r["confidence"].as_str().map(str::to_string);
                    g.evidence = r["evidence"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    g.cost_usd = v["usage"]["cost_usd"].as_f64();
                    g.input_tokens = v["usage"]["input_tokens"].as_u64();
                    g.output_tokens = v["usage"]["output_tokens"].as_u64();
                }
            }
            "spec_changed" => {
                if let Some(i) = attempt(&mut p) {
                    let a = &mut p.attempts[i];
                    a.spec_added =
                        Some(a.spec_added.unwrap_or(0) + v["lines_added"].as_u64().unwrap_or(0));
                    a.spec_removed = Some(
                        a.spec_removed.unwrap_or(0) + v["lines_removed"].as_u64().unwrap_or(0),
                    );
                }
            }
            "spec_rejected" => {
                let why = rejection(&v["reason"]);
                if let Some(i) = attempt(&mut p) {
                    p.attempts[i].rejected = Some(why.clone());
                }
                p.rejected.push((step, why));
            }
            "build_failed" => {
                p.build_log = v["log"].as_str().map(str::to_string);
                if let Some(i) = attempt(&mut p) {
                    p.attempts[i]
                        .build
                        .get_or_insert_with(BuildInfo::default)
                        .classification = v["classification"].as_str().map(str::to_string);
                }
            }
            "missing_packages" => {
                if let Some(i) = attempt(&mut p) {
                    let names: Vec<String> = v["names"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    p.attempts[i]
                        .build
                        .get_or_insert_with(BuildInfo::default)
                        .missing = names;
                }
            }
            "extract_result" => {
                if let Some(i) = attempt(&mut p) {
                    let e = &v["extract"];
                    let s = format!(
                        "{} (found by {}, genre {})",
                        e["phase"].as_str().unwrap_or("?"),
                        e["located_by"].as_str().unwrap_or("?"),
                        e["genre"].as_str().unwrap_or("?")
                    );
                    p.attempts[i]
                        .build
                        .get_or_insert_with(BuildInfo::default)
                        .extracts
                        .push(s);
                }
            }
            "notes_usage" => p.notes_cost += v["usage"]["cost_usd"].as_f64().unwrap_or(0.0),
            "warning" => {
                if let Some(m) = v["message"].as_str() {
                    p.warnings.push((step, m.to_string()));
                }
            }
            "package_finished" => p.finished = Some(v),
            _ => {}
        }
    }
    p
}

fn rejection(reason: &Value) -> String {
    let Value::Object(m) = reason else {
        return label(reason);
    };
    let Some((kind, detail)) = m.iter().next() else {
        return "spec rejected".to_string();
    };
    let extra = match kind.as_str() {
        "Placeholders" => detail["lines"].as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        }),
        "RpmspecParseFailed" => detail["stderr"]
            .as_str()
            .and_then(|s| s.lines().find(|l| l.starts_with("error:")))
            .map(|l| l.rsplit_once(": ").map_or(l, |(_, tail)| tail).to_string()),
        _ => (!detail.is_null()).then(|| detail.to_string()),
    };
    match extra {
        Some(e) if !e.is_empty() => format!("{kind}: {}", truncate(&e, 200)),
        _ => kind.clone(),
    }
}

fn command_line(argv: &[String]) -> String {
    let mut it = argv.iter().skip(1);
    let mut out = String::new();
    while let Some(a) = it.next() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(a);
        // Long paths of the work dir add nothing.
        if a == "--work-dir"
            && let Some(_) = it.next()
        {
            out.push_str(" ...");
        }
    }
    out
}

fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
    }
    None
}

fn budget(argv: Option<&[String]>) -> (f64, bool) {
    argv.and_then(|a| flag_value(a, "--budget-usd"))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or((DEFAULT_BUDGET_USD, true), |b| (b, false))
}

fn describe_reason(raw: &str) -> String {
    match raw {
        "ExplicitOverride" => "the package was named explicitly for this run (explicit override)",
        "FirstUnbuiltDependency" => "first unbuilt dependency in the build order",
        "RequestedRoot" => "requested as a root of the build",
        other => return other.to_string(),
    }
    .to_string()
}

fn version_info(p: &Parsed, dir: &Path, obs_name: &str) -> VersionInfo {
    let mut v = VersionInfo {
        selections: p.selected.len(),
        ..Default::default()
    };
    if let Some(s) = p.selected.last() {
        v.to = s["version"].as_str().map(str::to_string);
        v.from = s["from_version"].as_str().map(str::to_string);
        let raw = label(&s["reason"]);
        v.reason = Some(describe_reason(&raw));
        v.reason_raw = Some(raw);
        v.blocked_by = s["blocked_by"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
    }
    if let Some(argv) = &p.argv {
        // The run's `--only` limits which packages the flags belong to; only
        // claim them when the run was about this package.
        v.revision = flag_value(argv, "--revision");
        v.forced_version = flag_value(argv, "--to-version");
    }
    if let Some((spec, _)) = checkout(dir, obs_name) {
        v.spec_version = std::fs::read_to_string(spec).ok().and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("Version:"))
                .map(|x| x.trim().to_string())
        });
    }
    v
}

/// The checkout's spec and `.changes` (the directory is named after the OBS
/// project, so look in every subdirectory that is not the tool's own).
fn checkout(dir: &Path, obs_name: &str) -> Option<(PathBuf, PathBuf)> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "state" | "runs" | "rpms") || !e.path().is_dir() {
            continue;
        }
        let pkg_dir = e.path().join(obs_name);
        let spec = pkg_dir.join(format!("{obs_name}.spec"));
        if spec.is_file() {
            return Some((spec, pkg_dir.join(format!("{obs_name}.changes"))));
        }
    }
    None
}

fn latest_changes_entry(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = text.lines().peekable();
    // Skip the leading separator, then take up to the next one.
    if lines.peek().is_some_and(|l| l.starts_with("-----")) {
        lines.next();
    }
    lines
        .take_while(|l| !l.starts_with("-----"))
        .filter(|l| !l.trim().is_empty())
        .take(14)
        .map(|l| truncate(l, 200))
        .collect()
}

fn step_dir(run_pkg: &Path, step: u64) -> Option<PathBuf> {
    // Directories are `<step + 1>-<kind>`, zero-padded in newer runs.
    std::fs::read_dir(run_pkg).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let n: u64 = name.split('-').next()?.parse().ok()?;
        (n == step + 1 && e.path().is_dir()).then(|| e.path())
    })
}

fn outcome(
    p: &Parsed,
    state: Option<&Value>,
    dir: &Path,
    obs_name: &str,
    pkg: &str,
    run_pkg: Option<&Path>,
) -> Outcome {
    let mut o = Outcome {
        state_outcome: state
            .and_then(|s| s["last_outcome"].as_str())
            .map(str::to_string),
        state_reason: state
            .and_then(|s| s["last_failure_reason"].as_str())
            .map(str::to_string),
        ..Default::default()
    };
    match p.finished.as_ref().map(|f| &f["outcome"]) {
        Some(Value::String(s)) if s == "Built" => o.result = "built".into(),
        Some(Value::String(s)) if s == "AlreadyBuilt" => {
            o.result = "already built".into();
            o.detail = Some("nothing needed to be done in this run".into());
        }
        Some(Value::Object(m)) => {
            let (k, v) = m
                .iter()
                .next()
                .map(|(k, v)| (k.as_str(), v))
                .unwrap_or(("", &Value::Null));
            match k {
                "MissingDeps" => {
                    o.result = "missing dependencies".into();
                    let deps: Vec<&str> = v["deps"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    o.detail = Some(format!("waiting for: {}", deps.join(", ")));
                }
                "Failed" => {
                    o.result = "failed".into();
                    o.detail = v["reason"].as_str().map(str::to_string);
                }
                "Blocked" => {
                    o.result = "blocked".into();
                    o.detail = v["reason"].as_str().map(str::to_string);
                }
                other => o.result = other.to_string(),
            }
        }
        _ => o.result = "no result recorded".into(),
    }
    let shorten = |t: String| t.replace(&dir.display().to_string(), "<work dir>");
    o.detail = o.detail.take().map(shorten);
    o.state_reason = o.state_reason.take().map(shorten);
    if p.finished.is_none() {
        o.detail = Some(
            "this run wrote no package_finished event for it: it was interrupted, or is still going"
                .into(),
        );
    }
    if matches!(
        o.result.as_str(),
        "failed" | "no result recorded" | "missing dependencies"
    ) {
        let candidates = [
            p.build_log.as_ref().map(PathBuf::from),
            run_pkg.and_then(last_build_log),
            Some(dir.join("state").join(pkg).join("build.log")),
        ];
        for c in candidates.into_iter().flatten() {
            if let Some(text) = read_tail(&c) {
                let errors = log_errors(&text);
                if !errors.is_empty() {
                    o.log_source = Some(c.strip_prefix(dir).unwrap_or(&c).display().to_string());
                    o.log_errors = errors;
                    break;
                }
            }
        }
    }
    if let Some((_, changes)) = checkout(dir, obs_name) {
        o.changelog = latest_changes_entry(&changes);
    }
    o
}

fn last_build_log(run_pkg: &Path) -> Option<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(run_pkg)
        .ok()?
        .flatten()
        .map(|e| e.path().join("build.log"))
        .filter(|p| p.is_file())
        .collect();
    logs.sort_by_key(|p| {
        p.parent().and_then(|d| d.file_name()).and_then(|n| {
            n.to_string_lossy()
                .split('-')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
        })
    });
    logs.pop()
}

fn read_tail(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let max = 2 * 1024 * 1024;
    if len > max {
        f.seek(SeekFrom::Start(len - max)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The last error-looking lines of an OBS build log, without the `[  12s]`
/// time prefix, each once.
fn log_errors(log: &str) -> Vec<String> {
    const MARKS: [&str; 9] = [
        "error:",
        "Error",
        "ERROR",
        "FAILED",
        "fatal",
        "Bad exit status",
        "nothing provides",
        "cannot find",
        "undefined reference",
    ];
    let mut found: Vec<String> = Vec::new();
    for raw in log.lines() {
        let line = match raw.strip_prefix('[') {
            Some(rest) => rest.split_once(']').map_or(raw, |(_, t)| t),
            None => raw,
        }
        .trim();
        if line.is_empty() || line.starts_with("+ ") || !MARKS.iter().any(|m| line.contains(m)) {
            continue;
        }
        let line = truncate(line.strip_prefix("error: ").unwrap_or(line), 220);
        if !found.contains(&line) {
            found.push(line);
        }
    }
    let skip = found.len().saturating_sub(LOG_ERROR_LINES);
    found.split_off(skip)
}

fn flaky_lines(
    p: &Parsed,
    package_notes: &str,
    dir: &Path,
    pkg: &str,
    run: Option<&str>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in &p.attempts {
        let Some(d) = &a.dir else { continue };
        let Ok(prompt) = std::fs::read_to_string(Path::new(d).join("prompt.md")) else {
            continue;
        };
        let Some(section) = prompt.split("## Flaky tests (read by the pipeline)").nth(1) else {
            continue;
        };
        for l in section
            .lines()
            .skip(1)
            .take_while(|l| !l.starts_with("## "))
        {
            if l.starts_with("- Failed in") {
                out.push(format!(
                    "step {}: {}",
                    a.step,
                    truncate(l.trim_start_matches("- "), 300)
                ));
            }
        }
    }
    let state_notes =
        std::fs::read_to_string(dir.join("state").join(pkg).join("notes.md")).unwrap_or_default();
    for text in [package_notes, state_notes.as_str()] {
        for l in text
            .lines()
            .filter(|l| l.contains("flaky tests (failed in one of two builds)"))
        {
            let l = truncate(l.trim(), 300);
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    if let Some(probe) = read_json(&dir.join("state").join(pkg).join("flaky-probe.json"))
        && probe["run_id"].as_str() == run
        && let Some(first) = probe["first"].as_array()
    {
        let names: Vec<&str> = first.iter().filter_map(|x| x.as_str()).collect();
        out.push(format!(
            "probe: the first failing build failed {}; the retry has {}been compared",
            names.join(", "),
            if probe["compared"].as_bool() == Some(true) {
                ""
            } else {
                "not "
            }
        ));
    }
    out
}

/// Warnings worth reading: repeats are counted, the status-poll noise is
/// folded into one line and boilerplate is dropped.
fn warnings(p: &Parsed) -> Vec<WarningLine> {
    let mut out: Vec<WarningLine> = Vec::new();
    let mut polls: Option<(usize, String)> = None;
    for (step, m) in &p.warnings {
        if m.starts_with("`rpmspec --parse`") || m.starts_with("no path in the build log sits") {
            continue;
        }
        if m.starts_with("OBS build status still") {
            let n = polls.as_ref().map_or(0, |(n, _)| *n);
            polls = Some((n + 1, m.clone()));
            continue;
        }
        match out.iter_mut().find(|w| &w.message == m) {
            Some(w) => w.count += 1,
            None => out.push(WarningLine {
                step: *step,
                message: m.clone(),
                count: 1,
            }),
        }
    }
    if let Some((n, last)) = polls {
        out.push(WarningLine {
            step: None,
            message: if n > 1 {
                format!("{last} ({n} such status warnings; the last is shown)")
            } else {
                last
            },
            count: 1,
        });
    }
    for (step, why) in &p.rejected {
        out.push(WarningLine {
            step: *step,
            message: format!("the agent's spec was rejected and the previous one restored: {why}"),
            count: 1,
        });
    }
    out
}

fn unknowns(ex: &Explanation, has_state: bool) -> Vec<String> {
    let mut u = vec![
        "The agent's reasoning and evidence are the model's own summary of its turn, not a proof of what it intended or of why the build then behaved as it did.".to_string(),
        "No per-attempt changelog text is recorded: the changelog shown is the checkout's newest entry as it is now.".to_string(),
        "The cost counts the model turns the events report; time and cost of OBS builds are not included.".to_string(),
    ];
    if ex.run.is_none() {
        u.push("No run touched this package: only state and notes are shown.".into());
    } else if ex.run_info.finished.is_none() {
        u.push("The run wrote no run_finished event: it was interrupted or is still going, so the last step may be unfinished.".into());
    }
    if !has_state {
        u.push("There is no state.json for this package.".into());
    }
    if ex.cost.budget_is_default && ex.run.is_some() {
        u.push("The run's command line does not set --budget-usd: the budget shown is the default, not a recorded value.".into());
    }
    if ex.run_info.unreadable_lines > 0 {
        u.push(format!(
            "{} line(s) of run.jsonl could not be read and are left out.",
            ex.run_info.unreadable_lines
        ));
    }
    if ex.attempts.iter().any(|a| a.dir.is_none()) && ex.run.is_some() {
        u.push(
            "Some steps have no artifact directory (not written for that step, or cleaned up)."
                .into(),
        );
    }
    u
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('\u{2026}');
    t
}

// ---------------------------------------------------------------- rendering

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Heading,
    Label,
    Good,
    Bad,
    Warn,
    Dim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub kind: Kind,
}

pub type Line = Vec<Span>;

fn span(text: impl Into<String>, kind: Kind) -> Span {
    Span {
        text: text.into(),
        kind,
    }
}

/// Greedy word wrap by characters; a word longer than the line is broken.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut cur = String::new();
        let mut n = 0;
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            let mut w = word.chars().count();
            while w > width {
                if n > 0 {
                    lines.push(std::mem::take(&mut cur));
                    n = 0;
                }
                let head: String = word.chars().take(width).collect();
                word = word.chars().skip(width).collect();
                w -= width;
                lines.push(head);
            }
            if n > 0 && n + 1 + w <= width {
                cur.push(' ');
                cur.push_str(&word);
                n += 1 + w;
            } else {
                if n > 0 {
                    lines.push(std::mem::take(&mut cur));
                }
                cur = word;
                n = w;
            }
        }
        if n > 0 || para.trim().is_empty() {
            lines.push(cur);
        }
    }
    lines
}

struct Out {
    width: usize,
    lines: Vec<Line>,
}

impl Out {
    fn blank(&mut self) {
        self.lines.push(Vec::new());
    }
    fn heading(&mut self, text: &str) {
        if !self.lines.is_empty() {
            self.blank();
        }
        self.lines.push(vec![span(text, Kind::Heading)]);
    }
    /// Wrapped text with a hanging indent: `first` starts the first line.
    fn para(&mut self, indent: usize, first: &str, kind: Kind, text: &str) {
        let pad = " ".repeat(indent);
        let hang = " ".repeat(first.chars().count());
        let avail = self.width.saturating_sub(indent + first.chars().count());
        for (i, l) in wrap(text, avail).into_iter().enumerate() {
            let lead = if i == 0 { first } else { hang.as_str() };
            let mut line = vec![span(format!("{pad}{lead}"), Kind::Dim)];
            line.push(span(l, kind));
            self.lines.push(line);
        }
    }
    fn kv(&mut self, label: &str, kind: Kind, text: &str) {
        let first = format!("{label:<10} ");
        let pad = "  ";
        let hang = " ".repeat(first.chars().count());
        let avail = self.width.saturating_sub(2 + first.chars().count());
        for (i, l) in wrap(text, avail).into_iter().enumerate() {
            let mut line = Vec::new();
            if i == 0 {
                line.push(span(format!("{pad}{first}"), Kind::Label));
            } else {
                line.push(span(format!("{pad}{hang}"), Kind::Dim));
            }
            line.push(span(l, kind));
            self.lines.push(line);
        }
    }
}

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0 => format!("{ms}ms"),
        1..=59 => format!("{}.{}s", s, (ms % 1000) / 100),
        _ => format!("{}m{:02}s", s / 60, s % 60),
    }
}

fn kind_of_outcome(o: &str) -> Kind {
    match o {
        "Ok" | "built" | "already built" => Kind::Good,
        "Failed" | "failed" | "blocked" => Kind::Bad,
        _ => Kind::Warn,
    }
}

/// The report as styled lines wrapped to `width` columns.
pub fn render(ex: &Explanation, width: usize) -> Vec<Line> {
    let mut o = Out {
        width: width.max(30),
        lines: Vec::new(),
    };
    let mut head = String::new();
    let _ = write!(head, "{}", ex.package);
    o.lines.push(vec![
        span("Package ", Kind::Label),
        span(head, Kind::Heading),
    ]);
    o.kv("work dir", Kind::Dim, &ex.project_dir);
    match &ex.run {
        Some(r) => {
            let note = if ex.other_runs.is_empty() {
                String::new()
            } else {
                format!(
                    " (latest; {} other run(s) touched it: {}; pick one with --run)",
                    ex.other_runs.len(),
                    ex.other_runs
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            o.kv("run", Kind::Plain, &format!("{r}{note}"));
        }
        None => o.kv("run", Kind::Warn, "no run touched this package"),
    }
    if let Some(c) = &ex.run_info.command {
        o.kv("command", Kind::Dim, c);
    }
    if ex.run.is_some() {
        let (txt, kind) = match (&ex.run_info.finished, ex.run_info.elapsed_ms) {
            (Some(f), Some(ms)) => (format!("{f} after {}", fmt_ms(ms)), kind_of_outcome(f)),
            (Some(f), None) => (f.clone(), kind_of_outcome(f)),
            _ => (
                "not finished (interrupted, or still running)".to_string(),
                Kind::Warn,
            ),
        };
        o.kv("run ended", kind, &txt);
    }

    o.heading("Version");
    let v = &ex.version;
    match (&v.from, &v.to) {
        (Some(f), Some(t)) if f != t => o.kv("version", Kind::Plain, &format!("{f} -> {t}")),
        (_, Some(t)) => o.kv(
            "version",
            Kind::Plain,
            &format!("{t} (unchanged by this run)"),
        ),
        _ => o.kv(
            "version",
            Kind::Dim,
            "no target_selected event for it in this run",
        ),
    }
    if let Some(r) = &v.reason {
        let mut s = r.clone();
        if v.reason_raw.as_deref() != Some(r.as_str())
            && let Some(raw) = &v.reason_raw
        {
            let _ = write!(s, " [{raw}]");
        }
        o.kv("chosen", Kind::Plain, &s);
    }
    if !v.blocked_by.is_empty() {
        o.kv("blocked by", Kind::Warn, &v.blocked_by.join(", "));
    }
    if v.selections > 1 {
        o.kv(
            "selected",
            Kind::Dim,
            &format!(
                "{} times in this run (it came back after dependencies)",
                v.selections
            ),
        );
    }
    if let Some(r) = &v.revision {
        o.kv(
            "revision",
            Kind::Plain,
            &format!("{r} (from the run's --revision)"),
        );
    }
    if let Some(r) = &v.forced_version {
        o.kv(
            "pinned",
            Kind::Plain,
            &format!("{r} (from the run's --to-version)"),
        );
    }
    if let Some(s) = &v.spec_version {
        o.kv(
            "spec now",
            Kind::Dim,
            &format!("Version: {s} in the checkout"),
        );
    }

    o.heading("Outcome");
    let oc = &ex.outcome;
    o.kv("result", kind_of_outcome(&oc.result), &oc.result);
    if let Some(d) = &oc.detail {
        o.kv("detail", Kind::Plain, d);
    }
    if let Some(s) = &oc.state_outcome {
        let mut t = s.clone();
        if let Some(r) = &oc.state_reason {
            let _ = write!(t, ": {r}");
        }
        o.kv("state.json", Kind::Dim, &format!("latest state is {t}"));
    }
    if !oc.log_errors.is_empty() {
        o.kv(
            "log",
            Kind::Dim,
            &format!(
                "last error lines of {}",
                oc.log_source.as_deref().unwrap_or("the build log")
            ),
        );
        for l in &oc.log_errors {
            o.para(4, "| ", Kind::Bad, l);
        }
    }
    if !oc.changelog.is_empty() {
        o.kv(
            "changelog",
            Kind::Dim,
            "newest entry in the checkout's .changes",
        );
        for l in &oc.changelog {
            o.para(4, "", Kind::Plain, l);
        }
    }

    o.heading(&format!("Attempts ({})", ex.attempts.len()));
    if ex.attempts.is_empty() {
        o.para(
            2,
            "",
            Kind::Dim,
            "no step events recorded for this package in this run",
        );
    }
    for a in &ex.attempts {
        let mut title = format!("step {} {}", a.step, a.kind);
        if let Some(d) = &a.detail {
            let _ = write!(title, " ({d})");
        }
        if let Some(n) = a.attempt {
            let _ = write!(title, ", attempt {n}");
        }
        if let Some(r) = a.round {
            let _ = write!(title, ", round {r}");
        }
        if let Some(n) = a.agent_turn {
            let _ = write!(title, ", agent turn {n}");
        }
        let outc = a.outcome.clone().unwrap_or_else(|| "not finished".into());
        let mut tail = format!(" {outc}");
        if let Some(ms) = a.elapsed_ms {
            let _ = write!(tail, " {}", fmt_ms(ms));
        }
        if let Some(c) = a.agent.as_ref().and_then(|g| g.cost_usd) {
            let _ = write!(tail, " ${c:.4}");
        }
        let mut title_lines = wrap(&title, o.width.saturating_sub(2));
        let last = title_lines.pop().unwrap_or_default();
        for l in title_lines {
            o.lines
                .push(vec![span("  ", Kind::Plain), span(l, Kind::Label)]);
        }
        let fits = 2 + last.chars().count() + tail.chars().count() <= o.width;
        let mut line = vec![span("  ", Kind::Plain), span(last, Kind::Label)];
        if fits {
            line.push(span(tail, kind_of_outcome(&outc)));
            o.lines.push(line);
        } else {
            o.lines.push(line);
            o.lines.push(vec![
                span("    ", Kind::Plain),
                span(tail.trim_start().to_string(), kind_of_outcome(&outc)),
            ]);
        }
        if let Some(g) = &a.agent {
            let mut s = String::new();
            if let Some(t) = &g.task {
                let _ = write!(s, "task {t}");
            }
            if let Some(x) = &g.action {
                let _ = write!(s, "; action {x}");
            }
            if let Some(x) = &g.confidence {
                let _ = write!(s, "; confidence {x}");
            }
            if let (Some(i), Some(out)) = (g.input_tokens, g.output_tokens) {
                let _ = write!(s, "; {i} in / {out} out tokens");
            }
            o.para(4, "", Kind::Dim, s.trim_start_matches("; "));
            match &g.reasoning {
                Some(r) => o.para(4, "model says: ", Kind::Plain, &truncate(r, REASONING_MAX)),
                None => o.para(4, "", Kind::Dim, "no reply recorded for this turn"),
            }
            for e in g.evidence.iter().take(EVIDENCE_LINES) {
                o.para(6, "- ", Kind::Dim, &truncate(e, EVIDENCE_MAX));
            }
            if g.evidence.len() > EVIDENCE_LINES {
                o.para(
                    6,
                    "",
                    Kind::Dim,
                    &format!(
                        "... {} more evidence line(s) in response.json",
                        g.evidence.len() - EVIDENCE_LINES
                    ),
                );
            }
        }
        if let (Some(add), Some(rem)) = (a.spec_added, a.spec_removed) {
            o.para(4, "", Kind::Good, &format!("spec changed: +{add} -{rem}"));
        } else if a.agent.as_ref().is_some_and(|g| g.reasoning.is_some()) {
            o.para(4, "", Kind::Dim, "spec not changed by this turn");
        }
        if let Some(r) = &a.rejected {
            o.para(4, "", Kind::Bad, &format!("spec rejected: {r}"));
        }
        if let Some(b) = &a.build {
            if let Some(c) = &b.classification {
                o.para(
                    4,
                    "",
                    Kind::Bad,
                    &format!("build failure classified as {c}"),
                );
            }
            if !b.missing.is_empty() {
                o.para(
                    4,
                    "",
                    Kind::Warn,
                    &format!("missing packages: {}", b.missing.join(", ")),
                );
            }
            for e in &b.extracts {
                o.para(4, "", Kind::Dim, &format!("failure extracted from {e}"));
            }
        }
    }
    for s in &ex.skipped {
        o.para(2, "", Kind::Warn, s);
    }

    if !ex.flaky.is_empty() || ex.ladder.max_round.unwrap_or(0) > 0 || ex.ladder.reason.is_some() {
        o.heading("Flaky tests and ladder");
        for l in &ex.flaky {
            o.para(2, "flaky: ", Kind::Warn, l);
        }
        if ex.flaky.is_empty() {
            o.para(
                2,
                "",
                Kind::Dim,
                "no flaky-test verdict found in the prompts, notes or probe",
            );
        }
        if let Some(m) = ex.ladder.max_round {
            o.para(
                2,
                "",
                Kind::Plain,
                &format!("highest round counter in the step events: {m}"),
            );
        }
        if let Some(r) = &ex.ladder.reason {
            o.para(
                2,
                "ladder: ",
                Kind::Bad,
                &format!("every rung failed on the current spec: {r}"),
            );
        }
    }

    o.heading("Cost");
    let c = &ex.cost;
    let pct = if c.budget_usd > 0.0 {
        c.total_usd / c.budget_usd * 100.0
    } else {
        0.0
    };
    let mut s = format!(
        "${:.4} over {} agent turn(s), budget ${:.2}{} ({pct:.1}% used)",
        c.total_usd,
        c.turns,
        c.budget_usd,
        if c.budget_is_default {
            " (default)"
        } else {
            ""
        }
    );
    if let Some(sp) = c.state_spent_usd {
        let _ = write!(s, "; state.json counts ${sp:.4} for this run");
    }
    o.kv(
        "spent",
        if pct >= 100.0 { Kind::Bad } else { Kind::Plain },
        &s,
    );

    if !ex.warnings.is_empty() {
        o.heading("Warnings that mattered");
        for w in &ex.warnings {
            let mut t = String::new();
            if let Some(st) = w.step {
                let _ = write!(t, "[step {st}] ");
            }
            t.push_str(&w.message);
            if w.count > 1 {
                let _ = write!(t, " (x{})", w.count);
            }
            o.para(2, "! ", Kind::Warn, &t);
        }
    }

    o.heading("Notes kept");
    if !ex.notes.run_note_headings.is_empty() {
        o.kv(
            "this work",
            Kind::Dim,
            &format!(
                "state/{}/notes.md has {} entr(ies), across runs; the last: {}",
                ex.package,
                ex.notes.run_note_headings.len(),
                ex.notes.run_note_headings[ex.notes.run_note_headings.len().saturating_sub(3)..]
                    .join("; ")
            ),
        );
    }
    if ex.notes.package_notes.trim().is_empty() {
        o.para(2, "", Kind::Dim, "no package notes kept for this package");
    } else {
        let total = ex.notes.package_notes.lines().count();
        for l in ex.notes.package_notes.lines().take(NOTES_LINES) {
            if l.trim().is_empty() {
                o.blank();
            } else {
                let kind = if l.starts_with('#') {
                    Kind::Label
                } else {
                    Kind::Plain
                };
                o.para(2, "", kind, l);
            }
        }
        if total > NOTES_LINES {
            o.para(
                2,
                "",
                Kind::Dim,
                &format!(
                    "... {} more line(s) in {}.md in the notes directory",
                    total - NOTES_LINES,
                    ex.package
                ),
            );
        }
    }

    o.heading("What this report does not know");
    for u in &ex.unknown {
        o.para(2, "- ", Kind::Dim, u);
    }
    o.lines
}

/// Plain text, for tests and pipes.
pub fn plain(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// ANSI-coloured text when `color`, plain otherwise.
pub fn ansi(lines: &[Line], color: bool) -> String {
    let mut out = String::new();
    for l in lines {
        for s in l {
            let code = match s.kind {
                Kind::Plain => "",
                Kind::Heading => "1",
                Kind::Label => "36",
                Kind::Good => "32",
                Kind::Bad => "31",
                Kind::Warn => "33",
                Kind::Dim => "2",
            };
            if color && !code.is_empty() {
                let _ = write!(out, "\u{1b}[{code}m{}\u{1b}[0m", s.text);
            } else {
                out.push_str(&s.text);
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const RUN: &str = "20261027-175210-aaaa-1-0";

    fn events(extra: &[&str]) -> String {
        let mut v = vec![
            r#"{"event":"run_started","run_id":"R","argv":["auto-package","update","--project","p","--budget-usd","2.5","--work-dir","/w"],"work_dir":"/w"}"#.to_string(),
            r#"{"event":"target_selected","pkg":"foo","version":"2.0","reason":"FirstUnbuiltDependency","blocked_by":[],"from_version":"1.0"}"#.to_string(),
            r#"{"event":"target_selected","pkg":"other","version":"9","reason":"ExplicitOverride","blocked_by":[],"from_version":"9"}"#.to_string(),
            r#"{"pkg":"foo","event":"step_started","step":1,"kind":"AgentTurn","attempt":1,"round":0,"detail":"review"}"#.to_string(),
            r#"{"pkg":"foo","event":"agent_prompt","step":1,"task":{"FixAfterBuild":{"attempt":1,"max_attempts":10}}}"#.to_string(),
            r#"{"pkg":"foo","event":"agent_reply","step":1,"response":{"action":"Edited","reasoning":"Add a missing BuildRequires.","evidence":["Read build.log: nothing provides bar-devel"],"confidence":"High"},"usage":{"input_tokens":5,"output_tokens":100,"cost_usd":0.25}}"#.to_string(),
            r#"{"pkg":"foo","event":"spec_changed","step":1,"lines_added":3,"lines_removed":1}"#.to_string(),
            r#"{"pkg":"foo","event":"step_finished","step":1,"outcome":"Ok","elapsed":20555}"#.to_string(),
            r#"{"pkg":"foo","event":"warning","step":1,"message":"OBS build status still building after 132s with no change"}"#.to_string(),
            r#"{"pkg":"foo","event":"warning","step":1,"message":"OBS build status still building after 264s with no change"}"#.to_string(),
        ];
        v.extend(extra.iter().map(|s| s.to_string()));
        v.join("\n") + "\n"
    }

    fn fixture(extra: &[&str]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        write(&d.join("runs").join(RUN).join("run.jsonl"), &events(extra));
        write(
            &d.join("runs")
                .join(RUN)
                .join("foo/002-agent-review/response.json"),
            "{}",
        );
        write(
            &d.join("state/foo/state.json"),
            r#"{"last_outcome":"failed","last_failure_reason":"gave up","obs_name":"foo","cumulative_spent_usd":0.25,"cumulative_spent_run_id":"20261027-175210-aaaa-1-0"}"#,
        );
        write(
            &d.join("proj/foo/foo.spec"),
            "Name: foo\nVersion:        2.0\n",
        );
        write(
            &d.join("proj/foo/foo.changes"),
            "-------------------------------------------------------------------\nMon Oct 26 10:00:00 UTC 2026 - a@b.c\n\n- Update to 2.0\n\n-------------------------------------------------------------------\nOld\n\n- Old entry\n",
        );
        t
    }

    fn query<'a>(d: &'a Path) -> Query<'a> {
        Query {
            project_dir: d,
            package: "foo",
            run: None,
            package_notes: String::new(),
        }
    }

    #[test]
    fn a_failed_package_is_explained_from_its_files() {
        let t = fixture(&[
            r#"{"pkg":"foo","event":"step_started","step":2,"kind":"Build","attempt":1,"round":1,"detail":null}"#,
            r#"{"pkg":"foo","event":"build_failed","step":2,"classification":"CheckFailed","log":"LOG"}"#,
            r#"{"pkg":"foo","event":"step_finished","step":2,"outcome":"Failed","elapsed":61000}"#,
            r#"{"pkg":"foo","event":"package_finished","version":"2.0","outcome":{"Failed":{"reason":"gave up after 2 attempts"}}}"#,
            r#"{"event":"run_finished","outcome":"Failed","elapsed":70000}"#,
        ]);
        let d = t.path();
        write(
            &d.join("state/foo/build.log"),
            "[  5s] ok\n[ 10s] error: Bad exit status from /var/tmp/rpm-tmp (%check)\n[ 11s] + echo done\n",
        );
        let ex = explain(&query(d)).unwrap();
        assert_eq!(ex.run.as_deref(), Some(RUN));
        assert_eq!(ex.version.from.as_deref(), Some("1.0"));
        assert_eq!(ex.version.to.as_deref(), Some("2.0"));
        assert_eq!(ex.version.selections, 1);
        assert_eq!(ex.version.spec_version.as_deref(), Some("2.0"));
        assert_eq!(ex.outcome.result, "failed");
        assert_eq!(
            ex.outcome.detail.as_deref(),
            Some("gave up after 2 attempts")
        );
        assert_eq!(
            ex.outcome.log_errors,
            vec!["Bad exit status from /var/tmp/rpm-tmp (%check)"]
        );
        assert_eq!(ex.outcome.changelog.len(), 2);
        assert!(ex.outcome.changelog[1].contains("Update to 2.0"));
        assert_eq!(ex.attempts.len(), 2);
        let a = &ex.attempts[0];
        assert_eq!((a.spec_added, a.spec_removed), (Some(3), Some(1)));
        assert_eq!(a.agent_turn, Some(1));
        assert_eq!(
            a.agent.as_ref().unwrap().task.as_deref(),
            Some("FixAfterBuild 1/10")
        );
        assert!(a.dir.as_deref().unwrap().ends_with("002-agent-review"));
        assert_eq!(
            ex.attempts[1]
                .build
                .as_ref()
                .unwrap()
                .classification
                .as_deref(),
            Some("CheckFailed")
        );
        assert_eq!(ex.cost.total_usd, 0.25);
        assert_eq!(ex.cost.budget_usd, 2.5);
        assert!(!ex.cost.budget_is_default);
        assert_eq!(ex.cost.state_spent_usd, Some(0.25));
        // The other package's events and the two status polls are not mixed in.
        assert_eq!(ex.warnings.len(), 1);
        assert!(ex.warnings[0].message.contains("2 such status warnings"));
        assert_eq!(ex.run_info.finished.as_deref(), Some("Failed"));
    }

    #[test]
    fn an_interrupted_run_says_so_and_invents_no_outcome() {
        let t = fixture(&[]);
        let ex = explain(&query(t.path())).unwrap();
        assert_eq!(ex.outcome.result, "no result recorded");
        assert!(ex.run_info.finished.is_none());
        let text = plain(&render(&ex, 80));
        assert!(text.contains("not finished"), "{text}");
        assert!(text.contains("interrupted or is still going"), "{text}");
        assert!(text.contains("model's own summary"), "{text}");
    }

    #[test]
    fn the_latest_run_that_touched_the_package_is_the_default_and_run_picks_another() {
        let t = fixture(&[]);
        let d = t.path();
        let newer = "20261028-000000-bbbb-1-0";
        write(
            &d.join("runs").join(newer).join("run.jsonl"),
            "{\"event\":\"run_started\",\"argv\":[\"x\"]}\n",
        );
        let untouched = "20261029-000000-cccc-1-0";
        write(
            &d.join("runs").join(untouched).join("run.jsonl"),
            "{\"event\":\"run_started\",\"argv\":[\"x\"]}\n",
        );
        let ex = explain(&query(d)).unwrap();
        assert_eq!(
            ex.run.as_deref(),
            Some(RUN),
            "newer runs without the package are skipped"
        );
        let ex = explain(&Query {
            run: Some(newer),
            ..query(d)
        })
        .unwrap();
        assert_eq!(ex.run.as_deref(), Some(newer));
        assert_eq!(ex.other_runs, vec![RUN.to_string()]);
        assert!(
            explain(&Query {
                run: Some("nope"),
                ..query(d)
            })
            .is_err()
        );
    }

    #[test]
    fn missing_files_and_garbage_lines_are_tolerated() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        write(
            &d.join("runs/r1/run.jsonl"),
            "not json {\"pkg\":\"foo\"\n{\"pkg\":\"foo\",\"event\":\"step_started\",\"step\":0,\"kind\":\"Checkout\"}\n",
        );
        let ex = explain(&query(d)).unwrap();
        assert_eq!(ex.run_info.unreadable_lines, 1);
        assert_eq!(ex.attempts.len(), 1);
        assert!(ex.unknown.iter().any(|u| u.contains("no state.json")));
        let nothing = tempfile::tempdir().unwrap();
        assert!(
            explain(&query(nothing.path()))
                .unwrap_err()
                .contains("nothing about foo")
        );
        assert!(
            explain(&Query {
                package: "../x",
                ..query(d)
            })
            .is_err()
        );
    }

    #[test]
    fn flaky_verdicts_and_rejections_are_reported() {
        let t = fixture(&[
            r#"{"pkg":"foo","event":"spec_rejected","step":1,"reason":{"Placeholders":{"lines":["4: FIXME"]}},"restored_from":"/x"}"#,
            r#"{"pkg":"foo","event":"missing_packages","step":1,"names":["bar-devel"],"source":"BuildLog"}"#,
        ]);
        let d = t.path();
        write(
            &d.join("runs")
                .join(RUN)
                .join("foo/002-agent-review/prompt.md"),
            "x\n## Flaky tests (read by the pipeline)\n\nintro\n- Failed in ONE build only (flaky; convention 87): t1\n- Failed in BOTH builds (a real failure): t2\nDo not fix\n",
        );
        let ex = explain(&Query {
            package_notes: "- flaky tests (failed in one of two builds): t1; stable failures: t2\n"
                .into(),
            ..query(d)
        })
        .unwrap();
        assert_eq!(ex.flaky.len(), 3, "{:?}", ex.flaky);
        assert!(
            ex.warnings
                .iter()
                .any(|w| w.message.contains("rejected") && w.message.contains("FIXME"))
        );
        assert_eq!(
            ex.attempts[0].build.as_ref().unwrap().missing,
            vec!["bar-devel"]
        );
        assert_eq!(
            ex.attempts[0].rejected.as_deref(),
            Some("Placeholders: 4: FIXME")
        );
    }

    #[test]
    fn the_text_wraps_to_the_width_and_colours_only_when_asked() {
        let t = fixture(&[]);
        let notes = "# foo\n".to_string() + &"word ".repeat(80);
        let ex = explain(&Query {
            package_notes: notes,
            ..query(t.path())
        })
        .unwrap();
        for width in [40, 72] {
            let lines = render(&ex, width);
            for l in &lines {
                let n: usize = l.iter().map(|s| s.text.chars().count()).sum();
                assert!(n <= width, "{n} > {width}: {l:?}");
            }
        }
        let lines = render(&ex, 80);
        assert!(!ansi(&lines, false).contains('\u{1b}'));
        assert!(ansi(&lines, true).contains("\u{1b}[1m"));
        let json = serde_json::to_value(&ex).unwrap();
        assert_eq!(json["package"], "foo");
        assert_eq!(json["attempts"][0]["agent"]["action"], "Edited");
    }

    #[test]
    fn the_project_directory_is_resolved_like_the_other_commands() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path();
        write(&w.join("home_x_p-x86_64/state/foo/state.json"), "{}");
        write(&w.join("home_y_q-x86_64/state/bar/state.json"), "{}");
        let one = resolve_project_dir(w, None, "x86_64", "foo").unwrap();
        assert!(one.ends_with("home_x_p-x86_64"));
        assert!(resolve_project_dir(w, None, "x86_64", "nope").is_err());
        let by_name = resolve_project_dir(w, Some("home:x:p"), "x86_64", "foo").unwrap();
        assert!(by_name.ends_with("home_x_p-x86_64"));
        assert_eq!(
            resolve_project_dir(&one, None, "x86_64", "foo").unwrap(),
            one
        );
        write(&w.join("home_y_q-x86_64/state/foo/state.json"), "{}");
        assert!(
            resolve_project_dir(w, None, "x86_64", "foo")
                .unwrap_err()
                .contains("several")
        );
    }
}
