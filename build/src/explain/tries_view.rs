//! The tries as the explain report shows them: one package's tries in its
//! report, and the same counts for a whole run.

use super::*;
use serde::Serialize;

/// The events of run `run` that the tries are read from, and how many lines
/// of its `run.jsonl` named one and did not read.
pub(super) fn run_records(dir: &Path, run: &str) -> (Vec<PkgEvent>, usize) {
    let path = dir.join("runs").join(run).join("run.jsonl");
    records_from_jsonl(&std::fs::read_to_string(path).unwrap_or_default())
}

/// The tries section of one package's report, as lines (it starts with a
/// blank line). Empty when no run touched the package.
pub(super) fn tries_section(ex: &Explanation, width: usize) -> Vec<Line> {
    let Some(t) = &ex.tries else {
        return Vec::new();
    };
    let mut out: Vec<Line> = vec![
        Vec::new(),
        vec![span(format!("Tries ({})", t.tries), Kind::Heading)],
    ];
    let (head, kind) = match t.outcome {
        TriesOutcome::Built if t.tries == 0 => {
            ("built, with no build in this run".into(), Kind::Warn)
        }
        TriesOutcome::Built => (format!("{}, built", plural(t.tries)), Kind::Good),
        TriesOutcome::NotBuilt => (format!("{}, not built", plural(t.tries)), Kind::Bad),
        TriesOutcome::NoResult => (format!("{}, no result", plural(t.tries)), Kind::Warn),
        TriesOutcome::AlreadyBuilt => {
            ("no build in this run: OBS already had it".into(), Kind::Dim)
        }
    };
    out.push(vec![span("  ", Kind::Plain), span(head, kind)]);
    let reason = match t.outcome {
        TriesOutcome::Built => None,
        _ => t.reason.clone(),
    };
    if let Some(r) = reason {
        let first = match &t.shape {
            Some(s) => format!("reason [{s}]: "),
            None => "reason: ".to_string(),
        };
        let text = format!("{first}{r}");
        for l in wrap(&text, width.saturating_sub(4).max(8)) {
            out.push(vec![span("    ", Kind::Plain), span(l, Kind::Plain)]);
        }
    }
    for (i, a) in t.attempts.iter().enumerate() {
        let (text, kind) = match &a.reason {
            Some(r) => (format!("try {}: failed, {r}", i + 1), Kind::Bad),
            None => (
                format!("try {}: {}", i + 1, a.result),
                kind_of_try(&a.result),
            ),
        };
        let line = truncate(&text, width.saturating_sub(4).max(8));
        out.push(vec![span("  ", Kind::Plain), span(line, kind)]);
    }
    out
}

fn plural(n: usize) -> String {
    match n {
        1 => "1 try".to_string(),
        n => format!("{n} tries"),
    }
}

fn kind_of_try(result: &str) -> Kind {
    match result {
        "built" => Kind::Good,
        "skipped" => Kind::Dim,
        _ => Kind::Warn,
    }
}

/// The tries of every package a run started, for the whole-run view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunTries {
    pub run: String,
    /// Lines of the run's `run.jsonl` that named a build or package event and
    /// did not read.
    pub unreadable: usize,
    pub distribution: Distribution,
    pub packages: Vec<PkgTries>,
}

/// The project directory a whole-run view reads: `work_dir` itself when it
/// holds runs, the directory of `project` (or its branch project) when one is
/// given, otherwise the only project directory of `work_dir` that has runs.
pub fn resolve_run_dir(
    work_dir: &Path,
    project: Option<&str>,
    arch: &str,
) -> Result<PathBuf, String> {
    if project.is_some() || work_dir.join("runs").is_dir() {
        return resolve_project_dir(work_dir, project, arch, "");
    }
    let mut with_runs: Vec<PathBuf> = std::fs::read_dir(work_dir)
        .map_err(|e| format!("cannot read {}: {e}", work_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|d| d.join("runs").is_dir())
        .collect();
    with_runs.sort();
    match with_runs.len() {
        1 => Ok(with_runs.remove(0)),
        0 => Err(format!("no runs in {}", work_dir.display())),
        _ => Err(format!(
            "several projects have runs in {}: pass --project (one of: {})",
            work_dir.display(),
            with_runs
                .iter()
                .filter_map(|d| d.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The tries of every package of run `run` (default: the latest run of `dir`).
pub fn run_tries(dir: &Path, run: Option<&str>) -> Result<RunTries, String> {
    let run = match run {
        Some(r) if dir.join("runs").join(r).is_dir() => r.to_string(),
        Some(r) => {
            return Err(format!(
                "no run {r} in {} (runs: {})",
                dir.display(),
                list_runs(dir).join(", ")
            ));
        }
        None => list_runs(dir)
            .pop()
            .ok_or_else(|| format!("no run in {}", dir.display()))?,
    };
    let (records, unreadable) = run_records(dir, &run);
    let packages = per_package(&records);
    Ok(RunTries {
        run,
        unreadable,
        distribution: distribution(&packages),
        packages,
    })
}

/// The whole-run view as lines: the distribution, then the packages the
/// summary lists.
pub fn render_run(rt: &RunTries, width: usize) -> Vec<Line> {
    let width = width.max(30);
    let mut out: Vec<Line> = vec![vec![span(
        format!("Tries in run {}", rt.run),
        Kind::Heading,
    )]];
    let body = if ran_a_build(&rt.packages) {
        let d = rt.distribution;
        format!(
            "1 try: {}, 2 tries: {}, 3+ tries: {}, not built: {}",
            d.one, d.two, d.three_plus, d.not_built
        )
    } else {
        "no package started a build in this run".to_string()
    };
    out.push(vec![span("  ", Kind::Plain), span(body, Kind::Label)]);
    if rt.unreadable > 0 {
        out.push(vec![span(
            format!(
                "  {} line(s) of run.jsonl named a build or package event that could not be read: the counts may be short.",
                rt.unreadable
            ),
            Kind::Warn,
        )]);
    }
    let flagged = flagged(&rt.packages);
    out.push(Vec::new());
    out.push(vec![span(
        format!(
            "Not built, or built with 3 or more tries ({}), most tries first",
            flagged.len()
        ),
        Kind::Heading,
    )]);
    for p in flagged {
        let kind = match p.outcome {
            TriesOutcome::Built => Kind::Warn,
            _ => Kind::Bad,
        };
        for (i, l) in wrap(&describe(p), width.saturating_sub(4))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { "  " } else { "    " };
            out.push(vec![span(lead, Kind::Plain), span(l, kind)]);
        }
    }
    out
}
