//! Unified-diff parsing and a styled rendering of it for the TUI.
//!
//! Everything here is a pure function of the diff text (and a width), so it can
//! be tested without a terminal: [`parse`] turns `osc rdiff` / `diff -u` text
//! into [`Diff`] (files, hunks, line numbers), [`word_diff`] finds the changed
//! part of a modified line, and [`render`] turns a [`Diff`] into styled lines
//! in the spirit of `delta`: a rule under each file name, dimmed hunk headers
//! with their context highlighted, tinted added/removed lines with a +/-
//! gutter and old/new line numbers, and word-level emphasis.

use std::ops::Range;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

/// Diff lines read before the rest is dropped with a notice.
pub const MAX_DIFF_LINES: usize = 20_000;
/// Characters kept of one diff line.
pub const MAX_LINE_CHARS: usize = 2_000;
const TAB_STOP: usize = 4;
/// Tokens per side the word diff will compare (it is quadratic).
const MAX_WORD_TOKENS: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DLine {
    pub kind: Kind,
    pub text: String,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// The function/section part after the second `@@`.
    pub context: String,
    pub lines: Vec<DLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileDiff {
    pub old: String,
    pub new: String,
    pub hunks: Vec<Hunk>,
    /// Set for a file whose content is not shown: what to say instead.
    pub binary: Option<String>,
    pub added: usize,
    pub removed: usize,
}

impl FileDiff {
    /// The name shown for the file.
    pub fn title(&self) -> String {
        match (self.old.as_str(), self.new.as_str()) {
            ("/dev/null", n) => format!("{n} (new file)"),
            (o, "/dev/null") => format!("{o} (deleted)"),
            (o, n) if o == n || o.is_empty() => n.to_string(),
            (o, "") => o.to_string(),
            (o, n) => format!("{o} \u{2192} {n}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    /// Input lines past [`MAX_DIFF_LINES`] that were not read.
    pub dropped_lines: usize,
}

impl Diff {
    pub fn added(&self) -> usize {
        self.files.iter().map(|f| f.added).sum()
    }

    pub fn removed(&self) -> usize {
        self.files.iter().map(|f| f.removed).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// One line: files changed, +N, -M.
    pub fn summary(&self) -> String {
        if self.files.is_empty() {
            return "no changes".to_string();
        }
        let n = self.files.len();
        let binary = self.files.iter().filter(|f| f.binary.is_some()).count();
        let mut s = format!(
            "{n} file{} changed, +{} \u{2212}{}",
            if n == 1 { "" } else { "s" },
            self.added(),
            self.removed()
        );
        if binary > 0 {
            s.push_str(&format!(", {binary} binary"));
        }
        if self.dropped_lines > 0 {
            s.push_str(", truncated");
        }
        s
    }
}

/// Whether a file name is an archive or other binary that is never shown.
fn is_binary_name(name: &str) -> bool {
    const EXT: &[&str] = &[
        ".tar", ".gz", ".tgz", ".bz2", ".xz", ".zst", ".zip", ".whl", ".jar", ".rpm", ".png",
        ".jpg", ".jpeg", ".gif", ".ico", ".pdf", ".7z", ".lz", ".so", ".bin",
    ];
    let lower = name.to_ascii_lowercase();
    EXT.iter().any(|e| lower.ends_with(e))
}

/// Expand tabs to tab stops and replace other control characters, then cap
/// the length.
pub fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    let mut chars = 0;
    for c in line.chars() {
        if chars >= MAX_LINE_CHARS {
            out.push('\u{2026}');
            break;
        }
        chars += 1;
        match c {
            '\t' => {
                let n = TAB_STOP - col % TAB_STOP;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\r' => {}
            c if c.is_control() => {
                out.push('\u{b7}');
                col += 1;
            }
            c => {
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// A file name from a `---`/`+++` header: without the timestamp or revision
/// note after it and without the `a/` `b/` prefix.
fn header_name(rest: &str, prefix: &str) -> String {
    let rest = rest.split('\t').next().unwrap_or(rest);
    let rest = rest.split(" (").next().unwrap_or(rest).trim();
    rest.strip_prefix(prefix).unwrap_or(rest).to_string()
}

/// `@@ -a,b +c,d @@ context`.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, context) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (old_start, old_len) = range(old)?;
    let (new_start, new_len) = range(new)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        context: context.trim().to_string(),
        lines: Vec::new(),
    })
}

/// The file a `Binary file ... changed` line names.
fn binary_line_name(line: &str) -> String {
    for q in ['\'', '"'] {
        if let Some(a) = line.find(q)
            && let Some(len) = line[a + 1..].find(q)
        {
            return line[a + 1..a + 1 + len].to_string();
        }
    }
    // `Binary files a and b differ`
    let rest = line.trim_start_matches("Binary files ");
    let rest = rest.strip_suffix(" differ").unwrap_or(rest);
    let name = rest.rsplit_once(" and ").map_or(rest, |(_, b)| b);
    name.strip_prefix("b/").unwrap_or(name).trim().to_string()
}

/// Parse unified diff text. Lenient: lines it does not know (the section
/// banners `osc rdiff` prints, `Index:` lines) are skipped, and a diff
/// without any file header still parses as one anonymous file.
pub fn parse(text: &str) -> Diff {
    let mut diff = Diff::default();
    let mut file: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    let (mut rem_old, mut rem_new) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0usize, 0usize);

    fn close_hunk(file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        if let Some(h) = hunk.take()
            && let Some(f) = file.as_mut()
        {
            f.hunks.push(h);
        }
    }
    fn close_file(diff: &mut Diff, file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
        close_hunk(file, hunk);
        if let Some(mut f) = file.take() {
            let name = if f.new.is_empty() || f.new == "/dev/null" {
                f.old.clone()
            } else {
                f.new.clone()
            };
            if f.binary.is_none() && !f.hunks.is_empty() && is_binary_name(&name) {
                let bytes: usize = f
                    .hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .map(|l| l.text.len() + 1)
                    .sum();
                f.binary = Some(format!(
                    "Binary file {name} changed ({bytes} bytes of diff not shown)"
                ));
            }
            if f.binary.is_some() {
                f.hunks.clear();
                f.added = 0;
                f.removed = 0;
            }
            diff.files.push(f);
        }
    }

    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    for (n, &raw) in all.iter().enumerate() {
        if n >= MAX_DIFF_LINES {
            diff.dropped_lines = total - n;
            break;
        }
        let in_hunk = hunk.is_some() && (rem_old > 0 || rem_new > 0);
        if in_hunk {
            let (kind, body) = match raw.chars().next() {
                Some(' ') => (Some(Kind::Context), &raw[1..]),
                Some('-') => (Some(Kind::Del), &raw[1..]),
                Some('+') => (Some(Kind::Add), &raw[1..]),
                None => (Some(Kind::Context), ""),
                Some('\\') => continue,
                _ => (None, raw),
            };
            if let Some(kind) = kind {
                let (o, nw) = match kind {
                    Kind::Context => {
                        rem_old = rem_old.saturating_sub(1);
                        rem_new = rem_new.saturating_sub(1);
                        old_no += 1;
                        new_no += 1;
                        (Some(old_no - 1), Some(new_no - 1))
                    }
                    Kind::Del => {
                        rem_old = rem_old.saturating_sub(1);
                        old_no += 1;
                        if let Some(f) = file.as_mut() {
                            f.removed += 1;
                        }
                        (Some(old_no - 1), None)
                    }
                    Kind::Add => {
                        rem_new = rem_new.saturating_sub(1);
                        new_no += 1;
                        if let Some(f) = file.as_mut() {
                            f.added += 1;
                        }
                        (None, Some(new_no - 1))
                    }
                };
                if let Some(h) = hunk.as_mut() {
                    h.lines.push(DLine {
                        kind,
                        text: clean(body),
                        old_no: o,
                        new_no: nw,
                    });
                }
                continue;
            }
            // Not a hunk line: the hunk ended early; read it as a header.
        }
        if let Some(rest) = raw.strip_prefix("--- ")
            && all.get(n + 1).is_some_and(|next| next.starts_with("+++ "))
        {
            close_file(&mut diff, &mut file, &mut hunk);
            file = Some(FileDiff {
                old: header_name(rest, "a/"),
                ..FileDiff::default()
            });
            continue;
        }
        if let Some(rest) = raw.strip_prefix("+++ ") {
            if let Some(f) = file.as_mut()
                && f.new.is_empty()
                && f.hunks.is_empty()
                && hunk.is_none()
            {
                f.new = header_name(rest, "b/");
            }
            continue;
        }
        if raw.starts_with("@@ ") {
            if let Some(h) = parse_hunk_header(raw) {
                close_hunk(&mut file, &mut hunk);
                file.get_or_insert_with(|| FileDiff {
                    old: String::new(),
                    new: "(diff)".to_string(),
                    ..FileDiff::default()
                });
                (rem_old, rem_new) = (h.old_len, h.new_len);
                (old_no, new_no) = (h.old_start, h.new_start);
                hunk = Some(h);
            }
            continue;
        }
        if raw.starts_with("Binary file") {
            let name = binary_line_name(raw);
            let note = format!(
                "Binary file {name} {}",
                if raw.contains("added") {
                    "added"
                } else if raw.contains("deleted") || raw.contains("removed") {
                    "deleted"
                } else {
                    "changed"
                }
            );
            let same = file
                .as_ref()
                .is_some_and(|f| f.hunks.is_empty() && (f.new == name || f.old == name));
            if !same {
                close_file(&mut diff, &mut file, &mut hunk);
                file = Some(FileDiff {
                    old: name.clone(),
                    new: name,
                    ..FileDiff::default()
                });
            }
            if let Some(f) = file.as_mut() {
                f.binary = Some(note);
            }
            close_file(&mut diff, &mut file, &mut hunk);
        }
        // Anything else (`Index:`, `====`, `changes files:`, `old:`, rules) is
        // not part of a file's content.
    }
    close_file(&mut diff, &mut file, &mut hunk);
    diff
}

/// A diff from the one-line-per-change text the pipeline records for a spec
/// (`- old` / `+ new`, no hunks or line numbers).
pub fn parse_recorded(name: &str, text: &str) -> Diff {
    let mut file = FileDiff {
        old: name.to_string(),
        new: name.to_string(),
        ..FileDiff::default()
    };
    let mut hunk = Hunk {
        context: "recorded changes".to_string(),
        ..Hunk::default()
    };
    for raw in text.lines() {
        let (kind, body) = if let Some(b) = raw.strip_prefix("- ") {
            (Kind::Del, b)
        } else if let Some(b) = raw.strip_prefix("+ ") {
            (Kind::Add, b)
        } else {
            continue;
        };
        match kind {
            Kind::Del => file.removed += 1,
            _ => file.added += 1,
        }
        hunk.lines.push(DLine {
            kind,
            text: clean(body),
            old_no: None,
            new_no: None,
        });
        if hunk.lines.len() >= MAX_DIFF_LINES {
            break;
        }
    }
    let mut diff = Diff::default();
    if !hunk.lines.is_empty() {
        file.hunks.push(hunk);
        diff.files.push(file);
    }
    diff
}

/// Merge several recorded diffs (one per spec change of a run) into one.
pub fn parse_recorded_all(name: &str, diffs: &[String]) -> Diff {
    let mut all = Diff::default();
    for d in diffs {
        all.files.extend(parse_recorded(name, d).files);
    }
    all
}

/// Split `s` into word, space and punctuation tokens, as byte ranges.
fn tokens(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<(usize, u8)> = None;
    let class = |c: char| -> u8 {
        if c.is_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            0
        }
    };
    for (i, c) in s.char_indices() {
        let k = class(c);
        match start {
            Some((_, sk)) if sk == k && k != 0 => {}
            Some((st, _)) => {
                out.push(st..i);
                start = Some((i, k));
            }
            None => start = Some((i, k)),
        }
    }
    if let Some((st, _)) = start {
        out.push(st..s.len());
    }
    out
}

/// The parts of `old` and `new` that differ, as byte ranges in each, found by
/// a longest-common-subsequence over word tokens. `None` when the lines are
/// equal, share no word, or are too long to compare.
pub fn word_diff(old: &str, new: &str) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    if old == new {
        return None;
    }
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MAX_WORD_TOKENS || b.len() > MAX_WORD_TOKENS {
        return None;
    }
    let (n, m) = (a.len(), b.len());
    let mut t = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if old[a[i].clone()] == new[b[j].clone()] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let (mut ca, mut cb) = (vec![false; n], vec![false; m]);
    let (mut i, mut j) = (0, 0);
    let mut shared_word = false;
    while i < n && j < m {
        if old[a[i].clone()] == new[b[j].clone()] {
            ca[i] = true;
            cb[j] = true;
            if !old[a[i].clone()].trim().is_empty() {
                shared_word = true;
            }
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if !shared_word {
        return None;
    }
    let merge = |toks: &[Range<usize>], common: &[bool]| -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (tok, c) in toks.iter().zip(common) {
            if *c {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.end == tok.start => last.end = tok.end,
                _ => out.push(tok.clone()),
            }
        }
        out
    };
    Some((merge(&a, &ca), merge(&b, &cb)))
}

/// A diff drawn as styled lines, plus where its files and hunks start.
#[derive(Debug, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    /// The same lines as plain text, for searching.
    pub plain: Vec<String>,
    /// Index of the first line of each file's header.
    pub files: Vec<usize>,
    /// Index of each hunk header.
    pub hunks: Vec<usize>,
}

const ADD_BG: Color = Color::Rgb(16, 46, 24);
const ADD_EMPH: Color = Color::Rgb(30, 112, 52);
const DEL_BG: Color = Color::Rgb(58, 20, 24);
const DEL_EMPH: Color = Color::Rgb(140, 36, 46);

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// Break `cells` into rows of at most `width` columns. Always at least one row.
fn wrap_cells(cells: &[(char, bool)], width: usize) -> Vec<Vec<(char, bool)>> {
    let mut rows: Vec<Vec<(char, bool)>> = vec![Vec::new()];
    let mut used = 0;
    for &(c, e) in cells {
        let w = c.width().unwrap_or(0);
        if used + w > width && used > 0 {
            rows.push(Vec::new());
            used = 0;
        }
        rows.last_mut().unwrap().push((c, e));
        used += w;
    }
    rows
}

/// Draw `diff` for a viewport `width` columns wide. Long lines wrap.
pub fn render(diff: &Diff, width: usize) -> Rendered {
    let width = width.max(24);
    let max_no = diff
        .files
        .iter()
        .flat_map(|f| &f.hunks)
        .flat_map(|h| &h.lines)
        .flat_map(|l| [l.old_no, l.new_no])
        .flatten()
        .max()
        .unwrap_or(0);
    let nw = digits(max_no).max(3);
    let gutter_w = nw * 2 + 2;
    let body_w = width.saturating_sub(gutter_w + 2).max(8);
    let dim = Style::default().fg(Color::DarkGray);
    let mut out = Rendered::default();

    let push = |out: &mut Rendered, spans: Vec<Span<'static>>, bg: Option<Color>| {
        let mut spans = spans;
        let used: usize = spans
            .iter()
            .map(|s| {
                s.content
                    .chars()
                    .map(|c| c.width().unwrap_or(0))
                    .sum::<usize>()
            })
            .sum();
        out.plain
            .push(spans.iter().map(|s| s.content.as_ref()).collect());
        if let Some(bg) = bg
            && used < width
        {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
        out.lines.push(Line::from(spans));
    };

    for (fi, file) in diff.files.iter().enumerate() {
        if fi > 0 {
            push(&mut out, vec![Span::raw("")], None);
        }
        out.files.push(out.lines.len());
        let mut head = vec![Span::styled(
            format!("\u{25b8} {}", file.title()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )];
        if file.binary.is_none() {
            head.push(Span::styled(
                format!("  +{}", file.added),
                Style::default().fg(Color::Green),
            ));
            head.push(Span::styled(
                format!(" \u{2212}{}", file.removed),
                Style::default().fg(Color::Red),
            ));
        }
        push(&mut out, head, None);
        push(
            &mut out,
            vec![Span::styled(
                "\u{2500}".repeat(width),
                Style::default().fg(Color::Blue),
            )],
            None,
        );
        if let Some(note) = &file.binary {
            push(
                &mut out,
                vec![Span::styled(
                    format!("  {note}"),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::ITALIC),
                )],
                None,
            );
            continue;
        }
        for hunk in &file.hunks {
            out.hunks.push(out.lines.len());
            let mut spans = vec![Span::styled(
                format!(
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
                ),
                dim,
            )];
            if !hunk.context.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", hunk.context),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            push(&mut out, spans, None);

            // Pair the i-th removed line of a block with the i-th added line
            // that follows it, to highlight what changed inside the line.
            let mut emph: Vec<Vec<Range<usize>>> = vec![Vec::new(); hunk.lines.len()];
            let mut i = 0;
            while i < hunk.lines.len() {
                if hunk.lines[i].kind != Kind::Del {
                    i += 1;
                    continue;
                }
                let dels: Vec<usize> = (i..hunk.lines.len())
                    .take_while(|&k| hunk.lines[k].kind == Kind::Del)
                    .collect();
                let after = i + dels.len();
                let adds: Vec<usize> = (after..hunk.lines.len())
                    .take_while(|&k| hunk.lines[k].kind == Kind::Add)
                    .collect();
                for (d, a) in dels.iter().zip(&adds) {
                    if let Some((o, n)) = word_diff(&hunk.lines[*d].text, &hunk.lines[*a].text) {
                        emph[*d] = o;
                        emph[*a] = n;
                    }
                }
                i = after + adds.len();
            }

            for (li, line) in hunk.lines.iter().enumerate() {
                let (sign, sign_style, bg, emph_bg) = match line.kind {
                    Kind::Add => (
                        '+',
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                        Some(ADD_BG),
                        ADD_EMPH,
                    ),
                    Kind::Del => (
                        '-',
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        Some(DEL_BG),
                        DEL_EMPH,
                    ),
                    Kind::Context => (' ', Style::default(), None, Color::Reset),
                };
                let mut cells: Vec<(char, bool)> = Vec::with_capacity(line.text.len());
                for (bi, c) in line.text.char_indices() {
                    cells.push((c, emph[li].iter().any(|r| r.contains(&bi))));
                }
                let num = |n: Option<usize>| match n {
                    Some(n) => format!("{n:>nw$}"),
                    None => " ".repeat(nw),
                };
                let base = bg.map_or_else(Style::default, |b| Style::default().bg(b));
                for (ri, row) in wrap_cells(&cells, body_w).into_iter().enumerate() {
                    let gutter = if ri == 0 {
                        format!("{} {} ", num(line.old_no), num(line.new_no))
                    } else {
                        " ".repeat(gutter_w)
                    };
                    let mut spans = vec![
                        Span::styled(gutter, dim.patch(base)),
                        Span::styled(
                            format!("{} ", if ri == 0 { sign } else { ' ' }),
                            sign_style.patch(base),
                        ),
                    ];
                    let mut k = 0;
                    while k < row.len() {
                        let e = row[k].1;
                        let end = (k..row.len()).find(|&x| row[x].1 != e).unwrap_or(row.len());
                        let text: String = row[k..end].iter().map(|(c, _)| *c).collect();
                        let style = if e {
                            Style::default().bg(emph_bg).add_modifier(Modifier::BOLD)
                        } else {
                            base
                        };
                        spans.push(Span::styled(text, style));
                        k = end;
                    }
                    push(&mut out, spans, bg);
                }
            }
        }
    }
    if diff.dropped_lines > 0 {
        push(&mut out, vec![Span::raw("")], None);
        push(
            &mut out,
            vec![Span::styled(
                format!(
                    "\u{2026} diff truncated: {} more lines not shown",
                    diff.dropped_lines
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )],
            None,
        );
    }
    out
}

/// The first of `starts` after line `cur`.
pub fn next_start(starts: &[usize], cur: usize) -> Option<usize> {
    starts.iter().copied().find(|&s| s > cur)
}

/// The last of `starts` before line `cur`.
pub fn prev_start(starts: &[usize], cur: usize) -> Option<usize> {
    starts.iter().rev().copied().find(|&s| s < cur)
}

/// The next line (after `from` when `forward`, before it otherwise) whose text
/// holds `needle`, ignoring case; wraps around.
pub fn find_line(plain: &[String], needle: &str, from: usize, forward: bool) -> Option<usize> {
    if needle.is_empty() || plain.is_empty() {
        return None;
    }
    let needle = needle.to_lowercase();
    let n = plain.len();
    (1..=n)
        .map(|step| {
            if forward {
                (from + step) % n
            } else {
                (from + n - step % n) % n
            }
        })
        .find(|&i| plain[i].to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
changes files:
--------------
--- foo.changes
+++ foo.changes
@@ -1,2 +1,6 @@
+-------------------------------------------------------------------
+Mon Jan  1 00:00:00 UTC 2026 - Someone <a@b.c>
+
+- Update to 2.0
 -------------------------------------------------------------------
 Old entry
spec files:
-----------
--- foo.spec
+++ foo.spec
@@ -10,5 +10,5 @@ Name:           foo
 License:        MIT
 URL:            https://example.org
-Version:        1.0
+Version:        2.0
 Source0:        foo-%{version}.tar.gz
 BuildRequires:  gcc

Binary file 'foo-2.0.tar.gz' has changed.
";

    #[test]
    fn parses_files_hunks_and_counts() {
        let d = parse(SAMPLE);
        assert_eq!(d.files.len(), 3);
        assert_eq!(d.files[0].new, "foo.changes");
        assert_eq!((d.files[0].added, d.files[0].removed), (4, 0));
        assert_eq!((d.files[1].added, d.files[1].removed), (1, 1));
        assert_eq!(d.files[1].hunks[0].context, "Name:           foo");
        assert_eq!(d.added(), 5);
        assert_eq!(d.removed(), 1);
        assert_eq!(d.summary(), "3 files changed, +5 \u{2212}1, 1 binary");
    }

    #[test]
    fn numbers_old_and_new_lines_independently() {
        let d = parse(SAMPLE);
        let h = &d.files[1].hunks[0];
        let by = |t: &str| h.lines.iter().find(|l| l.text.starts_with(t)).unwrap();
        assert_eq!(
            (by("License").old_no, by("License").new_no),
            (Some(10), Some(10))
        );
        let del = by("Version:        1");
        assert_eq!(
            (del.kind, del.old_no, del.new_no),
            (Kind::Del, Some(12), None)
        );
        let add = by("Version:        2");
        assert_eq!(
            (add.kind, add.old_no, add.new_no),
            (Kind::Add, None, Some(12))
        );
        assert_eq!(by("BuildRequires").old_no, Some(14));
    }

    #[test]
    fn a_dashed_line_inside_a_hunk_is_content_not_a_header() {
        let d = parse("--- a.txt\n+++ a.txt\n@@ -1,2 +1,1 @@\n--- not a header\n keep\n");
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].hunks[0].lines[0].text, "-- not a header");
        assert_eq!(d.files[0].removed, 1);
    }

    #[test]
    fn binary_lines_and_archives_are_not_shown() {
        let d = parse(SAMPLE);
        assert_eq!(
            d.files[2].binary.as_deref(),
            Some("Binary file foo-2.0.tar.gz changed")
        );
        let d = parse("--- a.tar.gz\n+++ a.tar.gz\n@@ -1 +1 @@\n-aaaa\n+bbbb\n");
        let note = d.files[0].binary.as_deref().unwrap();
        assert!(note.starts_with("Binary file a.tar.gz changed ("), "{note}");
        assert!(d.files[0].hunks.is_empty());
        assert_eq!(d.added(), 0);
    }

    #[test]
    fn a_very_large_diff_is_capped_with_a_notice() {
        let mut text = String::from("--- a\n+++ a\n@@ -0,0 +1,30000 @@\n");
        for i in 0..30_000 {
            text.push_str(&format!("+line {i}\n"));
        }
        let d = parse(&text);
        assert!(d.dropped_lines > 9_000);
        assert!(d.summary().ends_with("truncated"));
        let r = render(&d, 80);
        assert!(r.plain.last().unwrap().contains("diff truncated"));
    }

    #[test]
    fn long_lines_are_capped_and_tabs_expand() {
        assert_eq!(clean("a\tb"), "a   b");
        assert_eq!(clean("\t"), "    ");
        let long = "x".repeat(5_000);
        assert!(clean(&long).chars().count() <= MAX_LINE_CHARS + 1);
        assert_eq!(clean("a\u{1}b"), "a\u{b7}b");
    }

    #[test]
    fn a_diff_without_headers_is_one_anonymous_file() {
        let d = parse("@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(d.files[0].new, "(diff)");
        assert_eq!(d.files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn empty_input_has_no_changes() {
        let d = parse("");
        assert!(d.is_empty());
        assert_eq!(d.summary(), "no changes");
    }

    #[test]
    fn new_and_deleted_files_are_titled() {
        let d = parse("--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+x\n");
        assert_eq!(d.files[0].title(), "n.txt (new file)");
    }

    #[test]
    fn recorded_spec_diffs_parse_without_numbers() {
        let d = parse_recorded_all("foo.spec", &["- Version: 1\n+ Version: 2\n".to_string()]);
        assert_eq!((d.added(), d.removed()), (1, 1));
        assert_eq!(d.files[0].hunks[0].lines[0].old_no, None);
        assert!(
            render(&d, 60)
                .plain
                .iter()
                .any(|l| l.contains("Version: 2"))
        );
    }

    #[test]
    fn word_diff_marks_only_the_changed_words() {
        let (o, n) = word_diff("Version:   1.0.3", "Version:   1.0.4").unwrap();
        assert_eq!(&"Version:   1.0.3"[o[0].clone()], "3");
        assert_eq!(&"Version:   1.0.4"[n[0].clone()], "4");
        assert!(word_diff("same", "same").is_none());
        assert!(word_diff("alpha", "beta").is_none());
    }

    #[test]
    fn rendering_shows_gutter_signs_and_navigation_points() {
        let d = parse(SAMPLE);
        let r = render(&d, 80);
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.hunks.len(), 2);
        assert!(r.plain[r.files[1]].contains("foo.spec"));
        let del = r
            .plain
            .iter()
            .find(|l| l.contains("Version:        1.0"))
            .unwrap();
        assert!(del.contains(" 12 ") && del.contains("- "), "{del}");
        let add = r
            .plain
            .iter()
            .find(|l| l.contains("Version:        2.0"))
            .unwrap();
        assert!(add.contains("+ "), "{add}");
        assert!(r.plain[r.hunks[1]].starts_with("@@ -10,5 +10,5 @@ Name:"));
        // A changed line carries emphasis on its changed part only.
        let line = r
            .lines
            .iter()
            .find(|l| l.to_string().contains("Version:        2.0"))
            .unwrap();
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.bg == Some(ADD_EMPH) && s.content == "2")
        );
        assert!(line.spans.iter().any(|s| s.style.bg == Some(ADD_BG)));
    }

    #[test]
    fn long_lines_wrap_to_the_width() {
        let text = format!("--- a\n+++ a\n@@ -1 +1 @@\n-x\n+{}\n", "word ".repeat(40));
        let r = render(&parse(&text), 40);
        assert!(r.lines.len() > 7);
        for l in &r.lines {
            assert!(l.width() <= 40, "{}", l.width());
        }
    }

    #[test]
    fn jumps_and_search() {
        let s = [3, 9, 20];
        assert_eq!(next_start(&s, 3), Some(9));
        assert_eq!(next_start(&s, 20), None);
        assert_eq!(prev_start(&s, 9), Some(3));
        assert_eq!(prev_start(&s, 3), None);
        let plain: Vec<String> = ["alpha", "Beta", "gamma", "beta2"].map(String::from).into();
        assert_eq!(find_line(&plain, "BETA", 0, true), Some(1));
        assert_eq!(find_line(&plain, "beta", 1, true), Some(3));
        assert_eq!(find_line(&plain, "beta", 3, true), Some(1));
        assert_eq!(find_line(&plain, "beta", 1, false), Some(3));
        assert_eq!(find_line(&plain, "zzz", 0, true), None);
    }
}
