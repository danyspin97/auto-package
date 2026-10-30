use super::*;

pub(super) fn render_detail(
    f: &mut Frame,
    state: &TuiState,
    visible: &[&String],
    area: Rect,
    selected: Option<usize>,
    browse: &BrowseState,
) -> RenderFeedback {
    if browse.show_lists {
        return render_lists(f, state, area, browse);
    }
    let pkg = selected
        .and_then(|i| visible.get(i))
        .and_then(|n| state.packages.get(n.as_str()));

    // A failure reason gets its own strip above the logs; without one the
    // panes take the whole area, as before.
    let body = match pkg.and_then(|p| p.fail_reason.as_ref().map(|r| (p, r))) {
        Some((pkg, reason)) => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(4), Constraint::Min(0)])
                .split(area);
            let title = match pkg.step.as_ref() {
                Some(step) if step.round > 1 => {
                    format!("Failed at {} (round {})", step.kind.label(), step.round)
                }
                Some(step) => format!("Failed at {}", step.kind.label()),
                None => "Failed".to_string(),
            };
            f.render_widget(
                Paragraph::new(reason.as_str())
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: true })
                    .block(
                        chrome_block()
                            .border_style(Style::default().fg(Color::Red))
                            .title_style(
                                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                            )
                            .title(format!("✗ {title}")),
                    ),
                rows[0],
            );
            rows[1]
        }
        None => area,
    };
    let [mut log_area, mut act_area] = two_panes(body, 65);
    // Two stacked panes each too short to read: show only the one in focus
    // (Tab switches), the log when the table has it.
    if body.width < SIDE_BY_SIDE_MIN_WIDTH && body.height < STACKED_PANES_MIN_HEIGHT {
        if browse.focus == Focus::Activity {
            (log_area, act_area) = (Rect::default(), body);
        } else {
            (log_area, act_area) = (body, Rect::default());
        }
    }

    let Some(pkg) = pkg else {
        f.render_widget(
            Paragraph::new("No package selected yet")
                .block(pane("Event Log", browse.focus == Focus::Log)),
            log_area,
        );
        f.render_widget(chrome_block().title("Agent Activity"), act_area);
        return RenderFeedback::default();
    };

    let log_viewport = viewport_height(log_area.height);
    // The filter narrows the log before windowing, so the offset and the
    // "more below" count are in filtered lines.
    let shown: Vec<usize> = match browse.filter.as_deref() {
        Some(needle) => filtered_indices(&pkg.log_lines, needle),
        None => (0..pkg.log_lines.len()).collect(),
    };
    let total = shown.len();
    let scroll = clamp_offset(browse.log_off, total, log_viewport);
    let log_items: Vec<ListItem> = shown[window(total, log_viewport, scroll)]
        .iter()
        .map(|&i| &pkg.log_lines[i])
        .map(|l| {
            let mut style = Style::default().fg(l.level.color());
            if l.emphasis {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(l.message.as_str()).style(style)
        })
        .collect();
    let filter_note = browse
        .filter
        .as_deref()
        .map(|needle| format!(" [filter: {needle}]"))
        .unwrap_or_default();
    let (more_note, short_more_note) = if scroll > 0 {
        (
            format!(" — {scroll} more below, End to follow"),
            format!(" ↓{scroll}"),
        )
    } else {
        (String::new(), String::new())
    };
    let skipped_note = if pkg.skipped.is_empty() {
        String::new()
    } else {
        let labels: Vec<&str> = pkg.skipped.iter().map(|(k, _)| k.label()).collect();
        format!(" (skipped: {})", labels.join(", "))
    };
    let log_title = first_fitting(
        vec![
            format!("Event Log{filter_note}{more_note}{skipped_note}"),
            format!("Event Log{filter_note}{more_note}"),
            format!("Event Log{filter_note}{short_more_note}"),
        ],
        pane_inner_width(log_area),
        |t| t.width(),
    )
    .unwrap_or_default();
    let log = List::new(log_items).block(pane(&log_title, browse.focus == Focus::Log));

    let act_viewport = viewport_height(act_area.height);
    let act_len = pkg.agent_activity.len();
    let act_sel = browse.act_sel.min(act_len.saturating_sub(1));
    let act_top = activity_top(act_sel, browse.act_top, act_len, act_viewport);
    let mut feedback = RenderFeedback {
        log_total: total,
        log_viewport,
        log_off: scroll,
        act_len,
        act_viewport,
        act_top,
        ..RenderFeedback::default()
    };

    // An expanded entry takes the Event Log's place, so its full text can be
    // read in the wider pane.
    let expanded = if browse.act_expanded {
        pkg.agent_activity.get(act_sel)
    } else {
        None
    };
    if let Some(entry) = expanded {
        let text_viewport = viewport_height(log_area.height);
        let text = wrap_text(&entry.text, pane_inner_width(log_area));
        let off = clamp_offset(browse.detail_off, text.len(), text_viewport);
        let end = (off + text_viewport).min(text.len());
        let lines: Vec<Line> = text[off..end]
            .iter()
            .map(|l| Line::styled(l.as_str(), Style::default().fg(entry.kind.color())))
            .collect();
        let title = format!("{} — Esc close, ↑/↓ scroll", entry.kind.label());
        f.render_widget(
            Paragraph::new(lines).block(pane(&title, browse.focus == Focus::Activity)),
            log_area,
        );
        feedback.detail_total = text.len();
        feedback.detail_viewport = text_viewport;
        feedback.detail_off = off;
    } else {
        f.render_widget(log, log_area);
    }

    let inner_w = pane_inner_width(act_area);
    let activity_items: Vec<ListItem> = pkg
        .agent_activity
        .iter()
        .skip(act_top)
        .take(act_viewport)
        .map(|a| {
            // Entries can span lines; the list shows the first one, and the
            // full text is one Enter away.
            let first = a.text.lines().next().unwrap_or("");
            ListItem::new(truncate_with_ellipsis(first, inner_w))
                .style(Style::default().fg(a.kind.color()))
        })
        .collect();
    let mut list_state = ListState::default();
    if browse.focus == Focus::Activity && act_len > 0 {
        list_state.select(Some(act_sel - act_top));
    }
    let position = if act_len > 0 {
        format!(" {}/{act_len}", act_sel + 1)
    } else {
        String::new()
    };
    let tokens = format!("{} in / {} out", pkg.input_tokens, pkg.output_tokens);
    let activity_title = first_fitting(
        vec![
            format!("Agent Activity{position} — {tokens}"),
            format!("Agent Activity{position}"),
            "Activity".to_string(),
        ],
        pane_inner_width(act_area),
        |t| t.width(),
    )
    .unwrap_or_default();
    let activity = List::new(activity_items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(pane(&activity_title, browse.focus == Focus::Activity));
    f.render_stateful_widget(activity, act_area, &mut list_state);
    feedback
}

/// `name <- dep1, dep2` for each package waiting on dependencies, in table
/// order; just `name` when the outcome named none.
pub(super) fn queued_lines(state: &TuiState) -> Vec<String> {
    state
        .order
        .iter()
        .filter_map(|n| state.packages.get(n.as_str()).map(|p| (n, p)))
        .filter(|(_, p)| p.status == PkgStatus::MissingDeps)
        .map(|(n, p)| {
            if p.waiting_on.is_empty() {
                n.clone()
            } else {
                let deps: Vec<String> = p
                    .waiting_on
                    .iter()
                    .map(|dep| waiting_label(state, dep))
                    .collect();
                format!("{n} <- {}", deps.join(", "))
            }
        })
        .collect()
}

/// A dependency a package waits on, as the table knows it: the same package
/// has a row under its RPM name (`python-logfire` for `logfire`), so without
/// its status here it would read as a second entry for one package.
fn waiting_label(state: &TuiState, dep: &str) -> String {
    let key = ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(dep));
    let row = state
        .order
        .iter()
        .filter_map(|n| state.packages.get(n.as_str()).map(|p| (n, p)))
        .find(|(n, _)| ap_build::osc::fuzzy_key(ap_build::osc::strip_rpm_prefix(n)) == key);
    match row {
        Some((_, p)) => format!("{dep} ({})", p.status.label()),
        None => dep.to_string(),
    }
}

/// `name version` for each package that is built, in table order.
pub(super) fn built_lines(state: &TuiState) -> Vec<String> {
    state
        .order
        .iter()
        .filter_map(|n| state.packages.get(n.as_str()).map(|p| (n, p)))
        .filter(|(_, p)| {
            matches!(
                p.status,
                PkgStatus::Succeeded | PkgStatus::BuiltLocally | PkgStatus::AlreadyBuilt
            )
        })
        .map(|(n, p)| format!("{n} {}", p.version_label()).trim_end().to_string())
        .collect()
}

/// The Queued and Built lists side by side (stacked on a narrow terminal) in
/// place of the detail panes. One offset is kept per list and clamped to that
/// list's own length.
fn render_lists(
    f: &mut Frame,
    state: &TuiState,
    area: Rect,
    browse: &BrowseState,
) -> RenderFeedback {
    let queued = queued_lines(state);
    let built = built_lines(state);
    let [queued_area, built_area] = two_panes(area, 50);
    // Both lists page by the same amount, the smaller pane's height, so one
    // offset per list means the same thing whichever list is scrolled.
    let viewport = viewport_height(queued_area.height).min(viewport_height(built_area.height));
    for (lines, title, col, off) in [
        (
            &queued,
            format!("Queued ({})", queued.len()),
            queued_area,
            clamp_offset(browse.queued_off, queued.len(), viewport),
        ),
        (
            &built,
            format!("Built ({})", built.len()),
            built_area,
            clamp_offset(browse.built_off, built.len(), viewport),
        ),
    ] {
        let end = (off + viewport).min(lines.len());
        let items: Vec<ListItem> = lines[off.min(end)..end]
            .iter()
            .map(|l| ListItem::new(l.as_str()))
            .collect();
        f.render_widget(
            List::new(items).block(pane(&title, browse.focus == Focus::Log)),
            col,
        );
    }
    RenderFeedback {
        queued_total: queued.len(),
        built_total: built.len(),
        lists_viewport: viewport,
        ..RenderFeedback::default()
    }
}

/// One line of a text prompt: label, the text typed so far with a cursor, and
/// the keys that finish or cancel it.
pub(super) fn prompt_line(label: &str, buf: &str, keys: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(label.to_string(), Style::default().fg(Color::Cyan)),
        Span::raw(buf.to_string()),
        Span::styled("█", Style::default().fg(Color::Cyan)),
        Span::styled(keys.to_string(), Style::default().fg(Color::DarkGray)),
    ])
}

pub(super) fn render_footer(
    f: &mut Frame,
    state: &TuiState,
    input: Option<&str>,
    browse: &BrowseState,
    area: Rect,
    retryable: bool,
    hint: Option<&str>,
) {
    let prompt = match (input, browse.filter_input.as_deref()) {
        (Some(buf), _) => Some((
            "add package (name or repo URL): ",
            "add: ",
            buf,
            "   Enter queue │ Esc cancel",
        )),
        (None, Some(buf)) => Some((
            "filter log: ",
            "filter: ",
            buf,
            "   Enter apply │ Esc cancel",
        )),
        (None, None) => None,
    };
    if let Some((label, short_label, buf, keys)) = prompt {
        // The cursor must stay on screen, so when even the short label does
        // not fit, only the end of what was typed is shown.
        let width = text_width(area);
        let line = first_fitting(
            vec![
                prompt_line(label, buf, keys),
                prompt_line(label, buf, ""),
                prompt_line(short_label, buf, ""),
                Line::from(vec![
                    Span::raw(suffix_within_width(buf, width.saturating_sub(1)).to_string()),
                    Span::styled("█", Style::default().fg(Color::Cyan)),
                ]),
            ],
            width,
            |l| l.width(),
        )
        .unwrap_or_default();
        f.render_widget(framed(area, Paragraph::new(line)), area);
        return;
    }
    // The hint, when there is one, is the most useful thing to say, so it
    // takes the place of the last log line.
    let last = match hint {
        Some(text) => Span::styled(text, Style::default().fg(LogLevel::Warn.color())),
        None => state
            .last_global_log
            .as_ref()
            .map(|l| Span::styled(l.message.as_str(), Style::default().fg(l.level.color())))
            .unwrap_or_else(|| Span::raw("")),
    };
    let nav = match browse.focus {
        Focus::Table => "↑/↓ select │ PgUp/PgDn/Home/End scroll log",
        Focus::Log => "↑/↓ PgUp/PgDn/Home/End scroll log │ / filter",
        Focus::Activity if browse.act_expanded => "↑/↓ PgUp/PgDn/Home/End scroll text",
        Focus::Activity => "↑/↓ PgUp/PgDn/Home/End move │ Enter expand",
    };
    // Esc does the first thing still open, so say what it does now.
    let quit = match esc_action(browse) {
        EscAction::ClosePrompt => "Esc cancel",
        EscAction::ClearFilter => "Esc clear filter │ q quit",
        EscAction::Collapse => "Esc close │ q quit",
        EscAction::Quit => "q/Esc quit",
    };
    let add = if state.accepts_requests {
        " │ n add package"
    } else {
        ""
    };
    let diff = if state.can_submit {
        " │ d diff │ e explain │ S submit │ A submit all"
    } else {
        " │ d diff │ e explain"
    };
    let hints = format!(
        "  {nav} │ Tab focus │ l lists{add}{diff}{} │ a {} succeeded │ {quit}",
        if retryable {
            " │ r retry (no space left)"
        } else {
            ""
        },
        if state.show_all { "hide" } else { "show" }
    );
    let dim = Style::default().fg(Color::DarkGray);
    // Fullest first: the log line and every key hint, then just the hints,
    // then the focus and quit keys, then quit alone.
    let line = first_fitting(
        vec![
            Line::from(vec![last, Span::styled(hints.clone(), dim)]),
            Line::from(Span::styled(hints, dim)),
            Line::from(Span::styled(format!("  Tab focus │ {quit}"), dim)),
            Line::from(Span::styled(format!("  {quit}"), dim)),
        ],
        text_width(area),
        |l| l.width(),
    )
    .unwrap_or_default();
    f.render_widget(framed(area, Paragraph::new(line)), area);
}
