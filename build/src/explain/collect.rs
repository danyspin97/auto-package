use super::*;

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
pub(super) struct Parsed {
    pub(super) argv: Option<Vec<String>>,
    pub(super) run_finished: Option<Value>,
    pub(super) selected: Vec<Value>,
    pub(super) attempts: Vec<Attempt>,
    pub(super) skipped: Vec<String>,
    pub(super) warnings: Vec<(Option<u64>, String)>,
    pub(super) rejected: Vec<(Option<u64>, String)>,
    pub(super) finished: Option<Value>,
    pub(super) build_log: Option<String>,
    pub(super) notes_cost: f64,
    pub(super) bad_lines: usize,
}

pub(super) fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub(super) fn list_runs(dir: &Path) -> Vec<String> {
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
pub(super) fn runs_touching(dir: &Path, pkg: &str) -> Vec<String> {
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

pub(super) fn label(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) if m.len() == 1 => m.keys().next().cloned().unwrap_or_default(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
