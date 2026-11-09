//! The tries as the explain report shows them: one package's tries in its
//! report, and the same counts for a whole run.

use super::*;

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
