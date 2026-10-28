use super::*;

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

pub(super) const ADD_BG: Color = Color::Rgb(16, 46, 24);
pub(super) const ADD_EMPH: Color = Color::Rgb(30, 112, 52);
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
