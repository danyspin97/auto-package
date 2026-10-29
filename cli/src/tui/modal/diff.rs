use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffSource {
    /// `osc rdiff` from the origin project to the branch.
    Origin,
    /// The spec changes this run recorded.
    Recorded,
}

pub enum DiffLoad {
    Loading,
    Failed(String),
    Ready(Diff),
}

pub struct DiffView {
    pub row: String,
    pub source: DiffSource,
    pub load: DiffLoad,
    pub scroll: Cell<usize>,
    pub search_input: Option<String>,
    pub search: Option<String>,
    pub started: Instant,
    /// Whether the run recorded spec changes to fall back on.
    pub has_recorded: bool,
    pub(super) seq: u64,
    pub(super) rendered: RefCell<Option<(usize, Rendered)>>,
    width: Cell<usize>,
    pub(super) viewport: Cell<usize>,
}

impl DiffView {
    pub(super) fn new(
        row: &str,
        source: DiffSource,
        load: DiffLoad,
        has_recorded: bool,
        seq: u64,
    ) -> Self {
        Self {
            row: row.to_string(),
            source,
            load,
            scroll: Cell::new(0),
            search_input: None,
            search: None,
            started: Instant::now(),
            has_recorded,
            seq,
            rendered: RefCell::new(None),
            width: Cell::new(80),
            viewport: Cell::new(20),
        }
    }

    pub(super) fn with_rendered<T>(
        &self,
        width: usize,
        f: impl FnOnce(&Rendered) -> T,
    ) -> Option<T> {
        let DiffLoad::Ready(d) = &self.load else {
            return None;
        };
        let mut cache = self.rendered.borrow_mut();
        if cache.as_ref().is_none_or(|(w, _)| *w != width) {
            *cache = Some((width, diffview::render(d, width)));
        }
        cache.as_ref().map(|(_, r)| f(r))
    }

    fn total(&self) -> usize {
        self.with_rendered(self.width.get(), |r| r.lines.len())
            .unwrap_or(0)
    }

    pub(super) fn max_scroll(&self) -> usize {
        max_offset(self.total(), self.viewport.get())
    }

    fn goto(&self, line: usize) {
        self.scroll.set(line.min(self.max_scroll()));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiffAction {
    None,
    Close,
    /// Ask for the diff again.
    Retry,
    /// Show the recorded spec changes instead.
    Recorded,
}

/// Apply one key to the diff viewer.
pub fn diff_key(v: &mut DiffView, code: KeyCode) -> DiffAction {
    if let Some(buf) = v.search_input.as_mut() {
        match prompt_key(buf, code) {
            PromptAction::Pending => {}
            PromptAction::Cancel => {
                if code == KeyCode::Enter {
                    v.search = None;
                }
                v.search_input = None;
            }
            PromptAction::Submit(text) => {
                v.search_input = None;
                v.search = Some(text);
                search_step(v, true, true);
            }
        }
        return DiffAction::None;
    }
    let page = v.viewport.get().saturating_sub(1).max(1);
    let cur = v.scroll.get();
    let width = v.width.get();
    match code {
        KeyCode::Char('q') => return DiffAction::Close,
        KeyCode::Esc => {
            if v.search.is_some() {
                v.search = None;
            } else {
                return DiffAction::Close;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => v.goto(cur + 1),
        KeyCode::Up | KeyCode::Char('k') => v.goto(cur.saturating_sub(1)),
        KeyCode::PageDown | KeyCode::Char(' ') => v.goto(cur + page),
        KeyCode::PageUp => v.goto(cur.saturating_sub(page)),
        KeyCode::Home | KeyCode::Char('g') => v.goto(0),
        KeyCode::End | KeyCode::Char('G') => v.goto(usize::MAX),
        KeyCode::Char(']') => hunk_jump(v, width, cur, true, false),
        KeyCode::Char('[') => hunk_jump(v, width, cur, false, false),
        KeyCode::Char('}') => hunk_jump(v, width, cur, true, true),
        KeyCode::Char('{') => hunk_jump(v, width, cur, false, true),
        KeyCode::Char('n') if v.search.is_some() => search_step(v, true, false),
        KeyCode::Char('N') if v.search.is_some() => search_step(v, false, false),
        KeyCode::Char('n') => hunk_jump(v, width, cur, true, false),
        KeyCode::Char('N') => hunk_jump(v, width, cur, false, false),
        KeyCode::Char('/') if matches!(v.load, DiffLoad::Ready(_)) => {
            v.search_input = Some(v.search.clone().unwrap_or_default());
        }
        KeyCode::Char('r') if v.source == DiffSource::Origin => return DiffAction::Retry,
        KeyCode::Char('s')
            if v.source == DiffSource::Origin
                && v.has_recorded
                && matches!(v.load, DiffLoad::Failed(_)) =>
        {
            return DiffAction::Recorded;
        }
        _ => {}
    }
    DiffAction::None
}

fn hunk_jump(v: &DiffView, width: usize, cur: usize, forward: bool, files: bool) {
    let target = v
        .with_rendered(width, |r| {
            let starts = if files { &r.files } else { &r.hunks };
            if forward {
                diffview::next_start(starts, cur)
            } else {
                diffview::prev_start(starts, cur)
            }
        })
        .flatten();
    if let Some(t) = target {
        v.goto(t);
    }
}

/// Move to the next (or previous) line holding the search text. `include_cur`
/// also accepts the line at the top of the view.
fn search_step(v: &DiffView, forward: bool, include_cur: bool) {
    let Some(needle) = v.search.clone() else {
        return;
    };
    let cur = v.scroll.get();
    let from = if include_cur {
        cur.checked_sub(1).unwrap_or(usize::MAX)
    } else {
        cur
    };
    let found = v
        .with_rendered(v.width.get(), |r| {
            let n = r.plain.len();
            if n == 0 {
                return None;
            }
            // `find_line` steps from `from`; a wrapped start of -1 is the last line.
            let from = if from == usize::MAX { n - 1 } else { from };
            diffview::find_line(&r.plain, &needle, from, forward)
        })
        .flatten();
    if let Some(i) = found {
        v.goto(i);
    }
}

/// Open the diff of `row`.
pub fn open_diff(
    state: &TuiState,
    browse: &mut BrowseState,
    row: &str,
    backend: Option<&Arc<dyn PackageBackend>>,
    jobs: &Sender<Job>,
) {
    let Some(p) = state.packages.get(row) else {
        return;
    };
    let has_recorded = !p.spec_diffs.is_empty();
    let seq = browse.next_seq();
    let Some(backend) = backend else {
        browse.modal = Some(Modal::Diff(recorded_view(row, p, seq)));
        return;
    };
    browse.modal = Some(Modal::Diff(DiffView::new(
        row,
        DiffSource::Origin,
        DiffLoad::Loading,
        has_recorded,
        seq,
    )));
    let (backend, jobs, row) = (backend.clone(), jobs.clone(), row.to_string());
    std::thread::spawn(move || {
        let result = backend.diff(&row);
        let _ = jobs.send(Job::Diff { row, seq, result });
    });
}

fn recorded_view(row: &str, p: &PkgState, seq: u64) -> DiffView {
    let diff = diffview::parse_recorded_all(&format!("{row}.spec"), &p.spec_diffs);
    DiffView::new(row, DiffSource::Recorded, DiffLoad::Ready(diff), true, seq)
}

/// Retry the origin diff, or switch to the recorded spec changes.
pub fn redo_diff(
    state: &TuiState,
    browse: &mut BrowseState,
    action: DiffAction,
    backend: Option<&Arc<dyn PackageBackend>>,
    jobs: &Sender<Job>,
) {
    let Some(Modal::Diff(v)) = &browse.modal else {
        return;
    };
    let row = v.row.clone();
    match action {
        DiffAction::Retry => open_diff(state, browse, &row, backend, jobs),
        DiffAction::Recorded => {
            if let Some(p) = state.packages.get(&row) {
                let seq = browse.next_seq();
                browse.modal = Some(Modal::Diff(recorded_view(&row, p, seq)));
            }
        }
        _ => {}
    }
}

pub(super) fn render_diff(f: &mut Frame, state: &TuiState, v: &DiffView, area: Rect) {
    f.render_widget(Clear, area);
    let source = match v.source {
        DiffSource::Origin => "origin \u{2192} branch",
        DiffSource::Recorded => "spec changes recorded by this run",
    };
    let version = state
        .packages
        .get(&v.row)
        .map(PkgState::version_label)
        .unwrap_or_default();
    let block = pane(&format!("Diff: {} {version} ({source})", v.row), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let [head, body, hints] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(inner);
    let width = inner.width as usize;
    v.width.set(width);
    v.viewport.set(body.height as usize);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let keys = "j/k PgUp/PgDn g/G scroll \u{2502} ]/[ hunk \u{2502} }/{ file \u{2502} / search \u{2502} q close";
    match &v.load {
        DiffLoad::Loading => {
            f.render_widget(Paragraph::new(Line::from(Span::styled("Diff", bold))), head);
            f.render_widget(
                Paragraph::new(format!(
                    "{} reading the diff from OBS (osc rdiff)\u{2026}",
                    spinner(v.started)
                ))
                .style(Style::default().fg(Color::Yellow)),
                body,
            );
            f.render_widget(Paragraph::new(hint_line("q/Esc close", width)), hints);
        }
        DiffLoad::Failed(e) => {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "Could not read the diff",
                    bold.fg(Color::Red),
                ))),
                head,
            );
            f.render_widget(
                Paragraph::new(e.clone())
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(Color::Red)),
                body,
            );
            let mut text = String::from("r retry");
            if v.has_recorded {
                text.push_str(" \u{2502} s show the spec changes recorded by this run");
            }
            text.push_str(" \u{2502} q close");
            f.render_widget(Paragraph::new(hint_line(&text, width)), hints);
        }
        DiffLoad::Ready(d) => {
            let total = v.total();
            let scroll = v.scroll.get().min(v.max_scroll());
            v.scroll.set(scroll);
            let pos = if total == 0 {
                String::new()
            } else {
                format!(
                    "  [{}-{}/{}]",
                    scroll + 1,
                    (scroll + body.height as usize).min(total),
                    total
                )
            };
            let found = v
                .search
                .as_deref()
                .map(|s| format!("  /{s}"))
                .unwrap_or_default();
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(d.summary(), bold),
                    Span::styled(
                        format!("{found}{pos}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])),
                head,
            );
            if d.is_empty() {
                let what = match v.source {
                    DiffSource::Origin => {
                        "No changes: the package is identical to the one in its origin project."
                    }
                    DiffSource::Recorded => {
                        "No changes recorded yet: this run has not changed the spec."
                    }
                };
                f.render_widget(
                    Paragraph::new(what).style(Style::default().fg(Color::Yellow)),
                    body,
                );
            } else {
                let needle = v.search.as_deref().map(str::to_lowercase);
                let lines = v
                    .with_rendered(width, |r| {
                        (scroll..(scroll + body.height as usize).min(r.lines.len()))
                            .map(|i| {
                                let mut line = r.lines[i].clone();
                                if let Some(n) = &needle
                                    && r.plain[i].to_lowercase().contains(n)
                                    && let Some(first) = line.spans.first_mut()
                                {
                                    first.style = first.style.add_modifier(Modifier::REVERSED);
                                }
                                line
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                f.render_widget(Paragraph::new(lines), body);
            }
            let line = match &v.search_input {
                Some(buf) => prompt_line("search: ", buf, "   Enter find \u{2502} Esc cancel"),
                None => hint_line(keys, width),
            };
            f.render_widget(Paragraph::new(line), hints);
        }
    }
}
