//! The report as plain text (no colour, safe in a pipe) and as JSON.

use std::fmt::Write;

use super::report::{AGENT_NOTE, Report, agent_failed_when_it_decides};
use super::{Finding, Severity};

/// `text` without the control, invisible and direction-changing characters a
/// package could use to move the cursor, clear the screen or reorder what is
/// read. A new line and a tab stay.
fn scrub(text: &str) -> String {
    text.chars()
        .filter(|c| matches!(c, '\n' | '\t') || !crate::pipeline::is_hidden(*c))
        .collect()
}

fn scrub_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => *s = scrub(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(scrub_value),
        serde_json::Value::Object(o) => o.values_mut().for_each(scrub_value),
        _ => {}
    }
}

/// The JSON of the report. JSON escapes a control character, but the text is
/// scrubbed all the same: what reads it may not.
pub fn render_json(report: &Report) -> String {
    let mut value = serde_json::to_value(report).expect("a report serialises");
    scrub_value(&mut value);
    serde_json::to_string_pretty(&value).expect("a value serialises") + "\n"
}

fn place(f: &Finding, many: bool) -> String {
    let mut out = String::new();
    if many && !f.package.is_empty() {
        let _ = write!(out, "{}: ", f.package);
    }
    if let Some(file) = &f.file {
        let _ = write!(out, "{file}");
        if let Some(line) = f.line {
            let _ = write!(out, ":{line}");
        }
        out.push_str(": ");
    }
    out
}

fn list(out: &mut String, title: &str, found: &[(&Finding, bool)]) {
    let _ = writeln!(out, "{title} ({})", found.len());
    let width = found.iter().map(|(f, _)| f.id.len()).max().unwrap_or(0);
    for (f, many) in found {
        let _ = writeln!(out, "  {:width$}  {}{}", f.id, place(f, *many), f.message);
    }
}

fn list_of(report: &Report, severity: Severity) -> Vec<(&Finding, bool)> {
    let many = report.actions.len() > 1;
    report
        .check_findings()
        .filter(|f| f.severity == severity)
        .map(|f| (f, many))
        .collect()
}

/// The text report.
pub fn render_text(report: &Report) -> String {
    let r = &report.request;
    let mut out = String::new();
    let when = r.created.replace('T', " ");
    let when = when.get(..16).unwrap_or(&when);
    let _ = writeln!(
        out,
        "Request {} ({}, by {}, created {when})",
        r.id, r.state, r.creator
    );
    for a in &report.actions {
        let rev = a
            .source
            .rev
            .as_deref()
            .map_or(String::new(), |v| format!("@{v}"));
        let _ = writeln!(
            out,
            "  submit {}/{}{rev} -> {}/{}",
            a.source.project, a.source.package, a.target.project, a.target.package
        );
        let version = match (&a.old_version, &a.new_version) {
            (Some(o), Some(n)) if o != n => format!("version {o} -> {n}; "),
            (_, Some(n)) => format!("version {n}; "),
            _ => String::new(),
        };
        let mut parts = Vec::new();
        for (word, names) in [
            ("changed", &a.changed),
            ("added", &a.added),
            ("removed", &a.removed),
        ] {
            if !names.is_empty() {
                parts.push(format!("{word}: {}", names.join(", ")));
            }
        }
        let _ = writeln!(out, "  {version}{}", parts.join("; "));
    }
    out.push('\n');
    list(&mut out, "Blockers", &list_of(report, Severity::Blocker));
    list(&mut out, "Warnings", &list_of(report, Severity::Warning));
    list(&mut out, "Notes", &list_of(report, Severity::Note));
    let skipped: Vec<_> = report.actions.iter().flat_map(|a| &a.skipped).collect();
    if !skipped.is_empty() {
        out.push_str("Not checked\n");
        for s in skipped {
            let _ = writeln!(out, "  {}: {}", s.check, s.why);
        }
    }
    if let Some(agent) = &report.agent {
        if agent.ran {
            let _ = writeln!(
                out,
                "Agent review (model opinion, not verified; {}, ${:.2})",
                agent.model.as_deref().unwrap_or("unknown model"),
                agent.cost_usd.unwrap_or(0.0)
            );
            for f in &agent.findings {
                let _ = writeln!(
                    out,
                    "  AGENT: {} {}{}",
                    f.severity.word(),
                    place(f, false),
                    f.message
                );
            }
            if agent.findings.is_empty() {
                out.push_str("  AGENT: no findings\n");
            }
            let _ = writeln!(out, "  {AGENT_NOTE}");
        } else {
            let why = agent.error.as_deref().unwrap_or("it did not run");
            let _ = writeln!(out, "Agent review: not run: {why}");
        }
    }
    if agent_failed_when_it_decides(report) {
        let _ = writeln!(
            out,
            "Result: the agent turn did not run and --fail-on-agent is given -> exit {}",
            report.exit
        );
        return scrub(&out);
    }
    let failing = report.failing();
    let from = if report.fail_on_agent {
        " (agent findings count)"
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "Result: {failing} finding{} at or above {}{from} -> exit {}",
        if failing == 1 { "" } else { "s" },
        report.fail_on.word(),
        report.exit
    );
    scrub(&out)
}
