use super::*;

pub(super) fn last_build_log(run_pkg: &Path) -> Option<PathBuf> {
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

pub(super) fn read_tail(path: &Path) -> Option<String> {
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
pub(super) fn log_errors(log: &str) -> Vec<String> {
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

pub(super) fn flaky_lines(
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
pub(super) fn warnings(p: &Parsed) -> Vec<WarningLine> {
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

pub(super) fn unknowns(ex: &Explanation, has_state: bool) -> Vec<String> {
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
    if ex.tries.as_ref().is_some_and(|t| t.tries > 0) {
        u.push(
            "run.jsonl does not record why the pipeline retried or gave up a package (its failure kind): a try shows the build log's classification only.".into(),
        );
    }
    if ex.attempts.iter().any(|a| a.dir.is_none()) && ex.run.is_some() {
        u.push(
            "Some steps have no artifact directory (not written for that step, or cleaned up)."
                .into(),
        );
    }
    u
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('\u{2026}');
    t
}
