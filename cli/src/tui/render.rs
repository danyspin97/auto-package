use super::*;

pub(super) fn render(
    f: &mut Frame,
    state: &TuiState,
    table_state: &mut TableState,
    input: Option<&str>,
    browse: &BrowseState,
    hint: Option<&str>,
) -> RenderFeedback {
    let area = f.area();
    if too_small(area) {
        render_too_small(f, area);
        return RenderFeedback::default();
    }
    let visible = state.visible_order();
    let has_run = !state.run_log.is_empty();
    let heights = stacked_heights(area.height, table_wanted(visible.len()), has_run);
    let [summary, table, detail, run, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints(heights.map(Constraint::Length))
        .areas(area);

    render_summary(f, state, summary);
    render_table(
        f,
        state,
        &visible,
        table,
        table_state,
        browse.focus == Focus::Table,
    );
    // The run's own log has a pane of its own below the package panes, so it
    // never shares one with a package's log.
    if has_run {
        render_run_log(f, state, run);
    }
    let feedback = render_detail(f, state, &visible, detail, table_state.selected(), browse);
    let hosts = ap_build::allowed_hosts::shared();
    let selected_name = table_state
        .selected()
        .and_then(|i| visible.get(i))
        .and_then(|name| state.packages.get_key_value(name.as_str()))
        .map(|(name, p)| (name.as_str(), p));
    let retry = selected_name
        .filter(|_| state.accepts_requests)
        .and_then(|(name, p)| modal::retry_label(name, p, &hosts));
    let allowable =
        selected_name.is_some_and(|(name, p)| !modal::pending_hosts(name, p, &hosts).is_empty());
    render_footer(f, state, input, browse, footer, retry, allowable, hint);
    if let Some(m) = &browse.modal {
        modal::render_modal(f, state, m, area);
    }
    feedback
}

/// Rows of the run log pane at most, borders included.
pub(super) const RUN_PANE_MAX_HEIGHT: u16 = 7;
/// Rows of the run log pane at least, borders included: a title and two lines.
pub(super) const RUN_PANE_MIN_HEIGHT: u16 = 4;
/// Body rows from which the run log pane sits beside a package table with at
/// least its border, header and one package row; shorter bodies give the run
/// log pane the first rows.
const RUN_BESIDE_TABLE_MIN_BODY: u16 = 8;

/// The newest run-level messages: the sweep that plans an `update` or audits
/// a project, which goes on while packages are being built.
fn render_run_log(f: &mut Frame, state: &TuiState, area: Rect) {
    let tail = window(state.run_log.len(), viewport_height(area.height), 0);
    let items: Vec<ListItem> = state
        .run_log
        .range(tail)
        .map(|l| ListItem::new(l.message.as_str()).style(Style::default().fg(l.level.color())))
        .collect();
    f.render_widget(List::new(items).block(pane("Run", false)), area);
}

/// Below this size nothing readable fits, so only a notice is drawn.
const MIN_WIDTH: u16 = 20;
const MIN_HEIGHT: u16 = 6;
/// Rows from which the summary and footer keep their bordered three-row
/// boxes; shorter terminals get them as one bare line each.
pub(super) const FULL_CHROME_MIN_HEIGHT: u16 = 20;
/// Width from which two panes sit side by side; narrower ones are stacked.
pub(super) const SIDE_BY_SIDE_MIN_WIDTH: u16 = 60;
/// Rows of detail area below which stacked panes are shown one at a time.
pub(super) const STACKED_PANES_MIN_HEIGHT: u16 = 10;

pub(super) fn too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

/// The one line drawn instead of the UI when the terminal is too small.
fn render_too_small(f: &mut Frame, area: Rect) {
    let width = area.width as usize;
    // The size and the minimum when they fit; the bare notice, which the
    // narrowest terminals get cut to the width, otherwise.
    let detailed = format!(
        "terminal too small ({}x{}, need {MIN_WIDTH}x{MIN_HEIGHT})",
        area.width, area.height
    );
    let line = if detailed.chars().count() <= width {
        detailed
    } else {
        truncate_with_ellipsis("terminal too small", width)
    };
    let row = Rect {
        height: area.height.min(1),
        ..area
    };
    f.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::Red)),
        row,
    );
}

/// Rows the package table wants: its header and border, plus one per package,
/// within a sensible range.
fn table_wanted(rows: usize) -> u16 {
    u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(3)
        .clamp(4, 12)
}

/// Heights of the summary, package table, detail pane, run log pane and
/// footer, top to bottom, for a terminal `height` rows tall. The summary and
/// footer shrink to a single bare line when the terminal is short.
///
/// Without a run log the table takes what it wants up to `table_cap` of the
/// body and the detail pane keeps the rest. With a log, and a body of at least
/// `RUN_BESIDE_TABLE_MIN_BODY` rows, the table keeps its border, header and one
/// package row (or its full want, up to the cap), the run log pane takes a
/// third of what is left (4 to 7 rows), and the detail pane keeps the remainder,
/// which can be 0 rows. Shorter bodies give the run log pane a quarter first.
pub(super) fn stacked_heights(height: u16, table_wanted: u16, has_run: bool) -> [u16; 5] {
    let chrome = if height >= FULL_CHROME_MIN_HEIGHT {
        3
    } else {
        1
    };
    let body = height.saturating_sub(2 * chrome);
    if !has_run {
        let table = table_wanted.min(table_cap(body));
        return [chrome, table, body - table, 0, chrome];
    }
    if body >= RUN_BESIDE_TABLE_MIN_BODY {
        let floor = table_wanted.min(4);
        let table = table_wanted.min(table_cap(body)).max(floor);
        let rest = body - table;
        let run = (rest / 3)
            .clamp(RUN_PANE_MIN_HEIGHT, RUN_PANE_MAX_HEIGHT)
            .min(rest);
        [chrome, table, rest - run, run, chrome]
    } else {
        let run = (body / 4)
            .clamp(RUN_PANE_MIN_HEIGHT, RUN_PANE_MAX_HEIGHT)
            .min(body);
        let rest = body - run;
        let table = table_wanted.min(table_cap(rest));
        [chrome, table, rest - table, run, chrome]
    }
}

/// Most rows the package table takes of a body of `body` rows: half of a body
/// of 24 rows or more, otherwise a third, and at least 4 rows (or half the body,
/// if that is less).
fn table_cap(body: u16) -> u16 {
    if body >= 24 {
        body / 2
    } else {
        (body / 3).max(4.min(body / 2))
    }
}

/// Splits `area` into two panes: side by side when the width allows, stacked
/// otherwise. `first` is the percentage of the space the first pane gets.
pub(super) fn two_panes(area: Rect, first: u16) -> [Rect; 2] {
    let direction = if area.width >= SIDE_BY_SIDE_MIN_WIDTH {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    Layout::default()
        .direction(direction)
        .constraints([
            Constraint::Percentage(first),
            Constraint::Percentage(100 - first),
        ])
        .areas(area)
}

/// The first of `options` at most `width` columns wide, or the last option
/// when none fits. Options are listed from the fullest to the most cut down.
pub(super) fn first_fitting<T>(
    options: Vec<T>,
    width: usize,
    measure: impl Fn(&T) -> usize,
) -> Option<T> {
    let mut last = None;
    for option in options {
        if measure(&option) <= width {
            return Some(option);
        }
        last = Some(option);
    }
    last
}

/// Columns left inside a bordered pane for its content.
pub(super) fn pane_inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

/// Whether a one-line strip is drawn in a box. Short terminals get the
/// summary and footer as bare lines instead.
fn is_boxed(area: Rect) -> bool {
    area.height >= 3
}

/// Columns of text that fit in a one-line strip, leaving out its border.
pub(super) fn text_width(area: Rect) -> usize {
    if is_boxed(area) {
        pane_inner_width(area)
    } else {
        area.width as usize
    }
}

/// `p` inside a border, when `area` is tall enough to hold one.
pub(super) fn framed(area: Rect, p: Paragraph<'_>) -> Paragraph<'_> {
    if is_boxed(area) {
        p.block(chrome_block())
    } else {
        p
    }
}

/// A bordered pane. The focused one gets a yellow border and a `▶` in its
/// title, so focus still shows without colour.
pub(super) fn pane(title: &str, focused: bool) -> Block<'static> {
    let block = chrome_block();
    if focused {
        block
            .border_style(Style::default().fg(Color::Yellow))
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .title(format!("▶ {title}"))
    } else {
        block.title(title.to_string())
    }
}

/// The bordered box every pane and strip is drawn in: rounded, with a dim
/// border so the content, not the frame, carries the eye.
pub(super) fn chrome_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
}

/// The run's scope as text, fullest first: every target, then the first few
/// with `(+N)` for the rest, then the project alone. Without targets the
/// whole project is the scope.
pub(super) fn scope_labels(scope: &RunScope) -> Vec<String> {
    let project = format!("project: {}", scope.project);
    let total = scope.targets.len();
    if total == 0 {
        return vec![format!("{project} | target: all packages"), project];
    }
    let mut labels: Vec<String> = (1..=total)
        .rev()
        .map(|shown| {
            let names = scope.targets[..shown].join(", ");
            let more = if shown < total {
                format!(" (+{})", total - shown)
            } else {
                String::new()
            };
            format!("{project} | target: {names}{more}")
        })
        .collect();
    labels.push(project);
    labels
}

/// The summary line: the scope in front of the counters. Every combination is
/// tried with the scope first, so the project and targets keep their place
/// while there is room for them beside the counters, and the counters shrink
/// before the scope does. Counters alone come last, for terminals too narrow
/// for both. Whatever is picked is cut to `width` as a last resort.
pub(super) fn header_line(scope: &[String], counters: Vec<String>, width: usize) -> String {
    let mut options: Vec<String> = scope
        .iter()
        .flat_map(|s| counters.iter().map(move |c| format!("{s} │{c}")))
        .collect();
    options.extend(counters);
    let line = first_fitting(options, width, |t| t.width()).unwrap_or_default();
    truncate_with_ellipsis(&line, width)
}
