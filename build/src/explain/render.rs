use super::*;

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

pub(super) fn span(text: impl Into<String>, kind: Kind) -> Span {
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

pub(super) struct Out {
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

pub(super) fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0 => format!("{ms}ms"),
        1..=59 => format!("{}.{}s", s, (ms % 1000) / 100),
        _ => format!("{}m{:02}s", s / 60, s % 60),
    }
}

pub(super) fn kind_of_outcome(o: &str) -> Kind {
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
    if let Some(b) = &ex.run_info.build {
        o.kv("build", Kind::Dim, b);
    }
    if !ex.run_info.hosts_allowed.is_empty() {
        o.kv(
            "hosts allowed",
            Kind::Warn,
            &ex.run_info.hosts_allowed.join(", "),
        );
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
