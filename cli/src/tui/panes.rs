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
    let skipped = c.skipped;
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
/// width is a flexible column that takes whatever the others leave. The status
/// is wide enough for `running (try 2/3, retrying)` behind its glyph; it says
/// the tries, so there is no column of its own for them.
pub(super) const TABLE_COLUMNS: [(&str, Option<u16>); 6] = [
    ("Status", Some(29)),
    ("Package", None),
    ("Version", Some(15)),
    ("Step", None),
    ("Time", Some(7)),
    ("Cost", Some(9)),
];
/// Column indices by importance, most important first. On a narrow terminal
/// the last ones are dropped first.
const TABLE_PRIORITY: [usize; 6] = [1, 0, 3, 2, 4, 5];
/// Least width a flexible column needs before it is worth showing.
const FLEX_MIN_WIDTH: u16 = 12;
/// The status column of a table too narrow for every column at the status's
/// full width: the cell is cut with an ellipsis, which keeps its glyph.
const NARROW_STATUS_WIDTH: u16 = 12;

/// Width of column `i` in a table `table_width` wide: the status column is
/// narrowed when the full one would cost the table a column.
pub(super) fn column_width(i: usize, table_width: u16) -> Option<u16> {
    let full = TABLE_COLUMNS[i].1;
    if i == 0 && table_width < table_needs(&[0, 1, 2, 3, 4, 5], None) {
        Some(NARROW_STATUS_WIDTH)
    } else {
        full
    }
}

/// Indices into `TABLE_COLUMNS` of the columns a table `width` wide shows, in
/// display order. Columns are added in priority order while they still fit,
/// and the package name is always shown.
pub(super) fn visible_columns(width: u16) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    for col in TABLE_PRIORITY {
        let mut trial = kept.clone();
        trial.push(col);
        trial.sort_unstable();
        if kept.is_empty() || table_needs(&trial, Some(width)) <= width {
            kept = trial;
        }
    }
    kept
}

/// Width a table showing `columns` needs: each fixed column's width (as in a
/// table `table_width` wide, or at full width), a minimum for each flexible
/// one, a space between columns and the borders.
fn table_needs(columns: &[usize], table_width: Option<u16>) -> u16 {
    let spaces = u16::try_from(columns.len().saturating_sub(1)).unwrap_or(u16::MAX);
    columns.iter().fold(spaces.saturating_add(2), |need, &i| {
        let width = match table_width {
            Some(w) => column_width(i, w),
            None => TABLE_COLUMNS[i].1,
        };
        need.saturating_add(width.unwrap_or(FLEX_MIN_WIDTH))
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
    let status_width = column_width(0, area.width).unwrap_or(0) as usize;
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
            let cells = [
                Cell::from(truncate_with_ellipsis(
                    &format!("{} {}", pkg.status.glyph(spin), pkg.status_cell()),
                    status_width,
                ))
                .style(Style::default().fg(pkg.status.color())),
                Cell::from(name.as_str()).style(
                    if matches!(
                        pkg.status,
                        PkgStatus::Queued
                            | PkgStatus::AlreadyBuilt
                            | PkgStatus::UpToDate
                            | PkgStatus::CheckSkipped
                    ) {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().add_modifier(Modifier::BOLD)
                    },
                ),
                Cell::from(pkg.version_label()).style(Style::default().fg(Color::Gray)),
                step_cell,
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

    let widths = columns.iter().map(|&i| match column_width(i, area.width) {
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
