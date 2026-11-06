use super::*;

pub(super) fn render_summary(f: &mut Frame, state: &TuiState, area: Rect) {
    let title = header_line(
        &scope_labels(&state.scope),
        summary_counters(state),
        text_width(area),
    );
    // The `│` dividers are dimmed so the scope and each counter group stand
    // apart; the text itself is unchanged.
    let strong = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    for (i, part) in title.split('│').enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(part.to_string(), strong));
    }
    let header = Paragraph::new(Line::from(spans));
    f.render_widget(framed(area, header), area);
}

/// The header's counter titles, widest first: what the sweep checked and
/// found (when there is a sweep), then the rows in each stage, the run's time
/// and cost. The narrower titles shorten the words before they drop parts.
pub(super) fn summary_counters(state: &TuiState) -> Vec<String> {
    let c = state.stage_counts();
    let skipped = c.skipped + state.sweep.map_or(0, |s| s.skipped);
    let elapsed = fmt_dur(state.run_start.map_or(Duration::ZERO, |s| s.elapsed()));
    let cost = state.total_cost_usd();
    // A long queue is what `--jobs` makes of it: only so many build at once.
    let queued = match state.scope.jobs {
        0 => format!("queued {}", c.queued),
        _ if c.queued == 0 => "queued 0".to_string(),
        jobs => format!("queued {} ({jobs} at a time)", c.queued),
    };
    let skipped_part = if skipped > 0 {
        format!(" · skipped {skipped}")
    } else {
        String::new()
    };
    // The last resolve's snapshot (see `ResolveFinished`): a dependency the
    // distro has never gets a row, so it is counted only here.
    let distro_part = match (state.resolve_total, state.resolve_in_distro) {
        (Some(total), Some(in_distro)) => format!(" · in distro {in_distro}/{total}"),
        _ => String::new(),
    };
    let sweep = |label: fn(&SweepCounts) -> String| {
        state
            .sweep
            .as_ref()
            .map_or(String::new(), |s| format!("{} · ", label(s)))
    };
    let full = format!(
        " {}{queued} · running {} · done {} · failed {} · waiting {}{skipped_part}{distro_part} │ elapsed {elapsed} │ cost ${cost:.2} ",
        sweep(SweepCounts::label),
        c.running,
        c.done,
        c.failed,
        c.waiting,
    );
    let short = format!(
        " {}queued {} · run {} · done {} · fail {} · wait {} │ ${cost:.2} ",
        sweep(SweepCounts::compact_label),
        c.queued,
        c.running,
        c.done,
        c.failed,
        c.waiting,
    );
    let shortest = format!(
        " run {} · queued {} · done {} · fail {} ",
        c.running, c.queued, c.done, c.failed
    );
    vec![full, short, shortest]
}

/// Package table columns as (header, fixed width), in display order. A `None`
/// width is a flexible column that takes whatever the others leave.
pub(super) const TABLE_COLUMNS: [(&str, Option<u16>); 7] = [
    ("Status", Some(11)),
    ("Package", None),
    ("Version", Some(15)),
    ("Step", None),
    ("Tries", Some(7)),
    ("Time", Some(7)),
    ("Cost", Some(9)),
];
/// Column indices by importance, most important first. On a narrow terminal
/// the last ones are dropped first.
const TABLE_PRIORITY: [usize; 7] = [1, 0, 3, 2, 5, 4, 6];
/// Least width a flexible column needs before it is worth showing.
const FLEX_MIN_WIDTH: u16 = 12;

/// Indices into `TABLE_COLUMNS` of the columns a table `width` wide shows, in
/// display order. Columns are added in priority order while they still fit,
/// and the package name is always shown.
pub(super) fn visible_columns(width: u16) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    for col in TABLE_PRIORITY {
        let mut trial = kept.clone();
        trial.push(col);
        trial.sort_unstable();
        if kept.is_empty() || table_needs(&trial) <= width {
            kept = trial;
        }
    }
    kept
}

/// Width a table showing `columns` needs: each fixed column's width, a
/// minimum for each flexible one, a space between columns and the borders.
fn table_needs(columns: &[usize]) -> u16 {
    let spaces = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(u16::MAX);
    columns.iter().fold(spaces.saturating_add(2), |need, &i| {
        need.saturating_add(TABLE_COLUMNS[i].1.unwrap_or(FLEX_MIN_WIDTH))
    })
}

pub(super) fn render_table(
    f: &mut Frame,
    state: &TuiState,
    visible: &[&String],
    area: Rect,
    table_state: &mut TableState,
    focused: bool,
) {
    let columns = visible_columns(area.width);
    let spin = state
        .run_start
        .map_or(0, |s| s.elapsed().as_millis() as u64 / 250);
    let rows: Vec<Row> = visible
        .iter()
        .map(|name| {
            let pkg = &state.packages[*name];
            let step_cell = match &pkg.sr {
                Some(mark) => Cell::from(Line::from(mark.span())),
                None => Cell::from(pkg.step_text()),
            };
            let tries = if pkg.max_tries > 0 {
                format!("{}/{}", pkg.tries, pkg.max_tries)
            } else {
                "-".to_string()
            };
            let cells = [
                Cell::from(format!("{} {}", pkg.status.glyph(spin), pkg.status_cell()))
                    .style(Style::default().fg(pkg.status.color())),
                Cell::from(name.as_str()).style(
                    if matches!(pkg.status, PkgStatus::Queued | PkgStatus::AlreadyBuilt) {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    },
                ),
                Cell::from(pkg.version_label()).style(Style::default().fg(Color::Gray)),
                step_cell,
                Cell::from(tries),
                Cell::from(pkg.time_cell()),
                Cell::from(format!("${:.2}", pkg.cost_usd)),
            ];
            let kept = cells
                .into_iter()
                .enumerate()
                .filter(|(i, _)| columns.contains(i))
                .map(|(_, cell)| cell);
            Row::new(kept)
        })
        .collect();

    let widths = columns.iter().map(|&i| match TABLE_COLUMNS[i].1 {
        Some(width) => Constraint::Length(width),
        None => Constraint::Fill(1),
    });
    let titles = TABLE_COLUMNS
        .iter()
        .enumerate()
        .filter(|(i, _)| columns.contains(i))
        .map(|(_, (title, _))| *title);

    let table = Table::new(rows, widths)
        .header(
            Row::new(titles).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(pane("Packages", focused));

    f.render_stateful_widget(table, area, table_state);
}
