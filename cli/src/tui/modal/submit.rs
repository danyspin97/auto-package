use super::*;

/// What the TUI asks of the command that owns the packages. The `update-project`
/// command implements it; other commands have no backend, and then the diff
/// falls back to the spec changes the run recorded and the submit keys say so.
pub trait PackageBackend: Send + Sync {
    /// The unified diff from the project the package was branched from to the
    /// branch (blocking: run on a thread).
    fn diff(&self, row: &str) -> Result<String, String>;
    /// What a submit request for the package would be, after the checks that
    /// decide whether one may be made (blocking).
    fn preview(&self, row: &str) -> Result<SubmitPreview, SubmitFailure>;
    /// Create the submit request; the request's id (blocking).
    fn submit(&self, row: &str) -> Result<String, SubmitFailure>;
}

/// The backend the event loop reads each frame, and whether the TUI stays open
/// after the run settles (so a request can still be made).
pub type BackendSlot = Arc<Mutex<Option<(Arc<dyn PackageBackend>, bool)>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitPreview {
    /// The `branch/pkg -> origin/pkg (old -> new)` line.
    pub summary: String,
    pub source: String,
    pub target: String,
    /// The request's description: what changed and the newest changelog entry.
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitFailure {
    /// An open request from the branch already exists (its id).
    Existing(String),
    Other(String),
}

/// What a row shows of its submit request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrMark {
    Submitting,
    Created(String),
    /// A request from the branch was already open.
    Existing(String),
    Failed(String),
}

impl SrMark {
    pub fn label(&self) -> String {
        match self {
            SrMark::Submitting => "submitting\u{2026}".to_string(),
            SrMark::Created(id) => format!("SR #{id}"),
            SrMark::Existing(id) => format!("SR #{id} (open)"),
            SrMark::Failed(why) => format!("submit failed: {why}"),
        }
    }

    fn color(&self) -> Color {
        match self {
            SrMark::Submitting => Color::Yellow,
            SrMark::Created(_) | SrMark::Existing(_) => Color::Green,
            SrMark::Failed(_) => Color::Red,
        }
    }

    /// The styled cell text.
    pub fn span(&self) -> Span<'static> {
        Span::styled(self.label(), Style::default().fg(self.color()))
    }
}

/// One package in the submit confirmation.
#[derive(Debug, Clone)]
pub struct SubmitEntry {
    pub row: String,
    pub preview: Result<SubmitPreview, String>,
    /// The diff summary line, or why there is none.
    pub diff_summary: String,
    /// A warning shown above the entry (a request already made for it).
    pub note: Option<String>,
    /// The id of the open request that rules the submit out.
    pub existing: Option<String>,
}

pub enum SubmitStage {
    Loading(Vec<String>),
    Ready(Vec<SubmitEntry>),
}

pub struct SubmitModal {
    pub stage: SubmitStage,
    pub scroll: Cell<usize>,
    total: Cell<usize>,
    viewport: Cell<usize>,
}

impl SubmitModal {
    fn eligible(&self) -> usize {
        match &self.stage {
            SubmitStage::Ready(es) => es.iter().filter(|e| e.preview.is_ok()).count(),
            SubmitStage::Loading(_) => 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SubmitAction {
    None,
    Close,
    Confirm,
}

pub fn submit_key(m: &mut SubmitModal, code: KeyCode) -> SubmitAction {
    let max = max_offset(m.total.get(), m.viewport.get());
    let page = m.viewport.get().saturating_sub(1).max(1);
    let cur = m.scroll.get();
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') if m.eligible() > 0 => {
            return SubmitAction::Confirm;
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') | KeyCode::Esc => {
            return SubmitAction::Close;
        }
        // With nothing to confirm there is nothing else to wait for.
        _ if matches!(&m.stage, SubmitStage::Ready(_)) && m.eligible() == 0 => {
            return SubmitAction::Close;
        }
        KeyCode::Down | KeyCode::Char('j') => m.scroll.set((cur + 1).min(max)),
        KeyCode::Up | KeyCode::Char('k') => m.scroll.set(cur.saturating_sub(1)),
        KeyCode::PageDown => m.scroll.set((cur + page).min(max)),
        KeyCode::PageUp => m.scroll.set(cur.saturating_sub(page)),
        KeyCode::Home => m.scroll.set(0),
        KeyCode::End => m.scroll.set(max),
        _ => {}
    }
    SubmitAction::None
}

fn is_built(p: &PkgState) -> bool {
    p.finished && matches!(p.status, PkgStatus::Succeeded | PkgStatus::AlreadyBuilt)
}

/// Whether the row may be offered for a submit request: it built, and no
/// request is being made for it.
fn submittable(p: &PkgState) -> bool {
    is_built(p) && p.sr != Some(SrMark::Submitting)
}

/// The rows a submit key offers: the selected one, or with `all` every built
/// row without a request.
pub fn submit_rows(state: &TuiState, selected: Option<&str>, all: bool) -> Vec<String> {
    if all {
        state
            .order
            .iter()
            .filter(|n| {
                let p = &state.packages[*n];
                submittable(p) && matches!(p.sr, None | Some(SrMark::Failed(_)))
            })
            .cloned()
            .collect()
    } else {
        selected.map(str::to_string).into_iter().collect()
    }
}

/// Start the checks for a submit confirmation of `rows`.
pub fn open_submit(
    state: &mut TuiState,
    browse: &mut BrowseState,
    rows: Vec<String>,
    backend: Option<&Arc<dyn PackageBackend>>,
    jobs: &Sender<Job>,
) {
    let Some(backend) = backend else {
        state.add_global_log(
            LogLevel::Warn,
            "Submitting is only available in update-project runs".to_string(),
        );
        return;
    };
    let mut ok = Vec::new();
    let mut notes = Vec::new();
    for row in rows {
        let Some(p) = state.packages.get(&row) else {
            continue;
        };
        if p.sr == Some(SrMark::Submitting) {
            state.add_global_log(
                LogLevel::Warn,
                format!("{row}: a submit request is already being made"),
            );
        } else if p.status == PkgStatus::BuiltLocally {
            state.add_global_log(
                LogLevel::Warn,
                format!("{row}: not submitted, it was built locally and OBS has no build of it"),
            );
        } else if !is_built(p) {
            state.add_global_log(
                LogLevel::Warn,
                format!(
                    "{row}: not submitted, it has not built successfully ({})",
                    p.status.label()
                ),
            );
        } else {
            notes.push(match &p.sr {
                Some(SrMark::Created(id)) => Some(format!(
                    "SR #{id} was already created for this package: this would be a second request"
                )),
                Some(SrMark::Existing(id)) => {
                    Some(format!("SR #{id} is already open for this package"))
                }
                Some(SrMark::Failed(why)) => Some(format!("the last attempt failed: {why}")),
                _ => None,
            });
            ok.push(row);
        }
    }
    if ok.is_empty() {
        if state.last_global_log.is_none() {
            state.add_global_log(LogLevel::Info, "No built package to submit".to_string());
        }
        return;
    }
    browse.modal = Some(Modal::Submit(SubmitModal {
        stage: SubmitStage::Loading(ok.clone()),
        scroll: Cell::new(0),
        total: Cell::new(0),
        viewport: Cell::new(0),
    }));
    let (backend, jobs) = (backend.clone(), jobs.clone());
    std::thread::spawn(move || {
        let entries = ok
            .into_iter()
            .zip(notes)
            .map(|(row, note)| {
                let preview = backend.preview(&row);
                let existing = match &preview {
                    Err(SubmitFailure::Existing(id)) => Some(id.clone()),
                    _ => None,
                };
                let diff_summary = match backend.diff(&row) {
                    Ok(text) => diffview::parse(&text).summary(),
                    Err(e) => format!("diff unavailable: {e}"),
                };
                SubmitEntry {
                    row,
                    preview: preview.map_err(|e| match e {
                        SubmitFailure::Existing(id) => format!("request #{id} is already open"),
                        SubmitFailure::Other(why) => why,
                    }),
                    diff_summary,
                    note,
                    existing,
                }
            })
            .collect();
        let _ = jobs.send(Job::Preview(entries));
    });
}

/// The user said yes: make the requests of the confirmed entries, one after
/// the other on a thread.
pub fn confirm_submit(
    state: &mut TuiState,
    browse: &mut BrowseState,
    backend: Option<&Arc<dyn PackageBackend>>,
    jobs: &Sender<Job>,
) {
    let Some(Modal::Submit(m)) = browse.modal.take() else {
        return;
    };
    let (SubmitStage::Ready(entries), Some(backend)) = (m.stage, backend) else {
        return;
    };
    let mut rows = Vec::new();
    for e in entries.into_iter().filter(|e| e.preview.is_ok()) {
        // Never twice at once, whatever the key repeat did.
        if let Some(p) = state.packages.get_mut(&e.row)
            && is_built(p)
            && p.sr != Some(SrMark::Submitting)
        {
            p.sr = Some(SrMark::Submitting);
            rows.push(e.row);
        }
    }
    if rows.is_empty() {
        return;
    }
    state.add_global_log(
        LogLevel::Info,
        format!("Submitting {} request(s)\u{2026}", rows.len()),
    );
    let (backend, jobs) = (backend.clone(), jobs.clone());
    std::thread::spawn(move || {
        for row in rows {
            let result = backend.submit(&row);
            let _ = jobs.send(Job::Submitted { row, result });
        }
    });
}

/// The confirmation text for the entries, wrapped to `width`.
pub fn submit_lines(entries: &[SubmitEntry], width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut out: Vec<Line<'static>> = Vec::new();
    let inner = width.saturating_sub(4).max(10);
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push(Line::from(""));
        }
        out.push(Line::from(Span::styled(
            format!("\u{25b8} {}", e.row),
            bold.fg(Color::Cyan),
        )));
        if let Some(note) = &e.note {
            for l in wrap_text(&format!("! {note}"), inner) {
                out.push(Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }
        match &e.preview {
            Err(why) => {
                for l in wrap_text(&format!("not submitted: {why}"), inner) {
                    out.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(Color::Red),
                    )));
                }
            }
            Ok(p) => {
                let field = |name: &str, value: &str| {
                    Line::from(vec![
                        Span::styled(format!("  {name:<8}"), dim),
                        Span::raw(value.to_string()),
                    ])
                };
                out.push(field("source", &format!("{} (branch project)", p.source)));
                out.push(field("target", &format!("{} (origin project)", p.target)));
                out.push(field("update", &p.summary));
                out.push(field("changes", &e.diff_summary));
                out.push(Line::from(Span::styled(
                    "  request text (with the changelog entry):",
                    dim,
                )));
                let desc = wrap_text(&p.description, inner.saturating_sub(2));
                let shown = desc.len().min(16);
                for l in &desc[..shown] {
                    out.push(Line::from(vec![
                        Span::styled("  \u{2502} ", dim),
                        Span::raw(l.clone()),
                    ]));
                }
                if desc.len() > shown {
                    out.push(Line::from(Span::styled(
                        format!("  \u{2502} \u{2026} {} more lines", desc.len() - shown),
                        dim,
                    )));
                }
            }
        }
    }
    out
}

pub(super) fn render_submit(f: &mut Frame, m: &SubmitModal, area: Rect) {
    f.render_widget(Clear, area);
    let block = pane("Submit request", true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let [body, hints] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .areas(inner);
    let width = inner.width as usize;
    match &m.stage {
        SubmitStage::Loading(rows) => {
            f.render_widget(
                Paragraph::new(format!(
                    "\u{280b} checking {} for a submit request (build status, open requests, diff)\u{2026}",
                    rows.join(", ")
                ))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Yellow)),
                body,
            );
            f.render_widget(Paragraph::new(hint_line("n/Esc cancel", width)), hints);
        }
        SubmitStage::Ready(entries) => {
            let lines = submit_lines(entries, width);
            m.total.set(lines.len());
            m.viewport.set(body.height as usize);
            let scroll = m
                .scroll
                .get()
                .min(max_offset(lines.len(), body.height as usize));
            m.scroll.set(scroll);
            let shown: Vec<Line> = lines
                .into_iter()
                .skip(scroll)
                .take(body.height as usize)
                .collect();
            f.render_widget(Paragraph::new(shown), body);
            let n = m.eligible();
            let text = if n > 0 {
                format!(
                    "y create {n} submit request{} \u{2502} n/Esc cancel \u{2502} j/k PgUp/PgDn scroll",
                    if n == 1 { "" } else { "s" }
                )
            } else {
                "nothing can be submitted \u{2502} any key closes".to_string()
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate_with_ellipsis(&text, width),
                    Style::default().fg(if n > 0 { Color::Green } else { Color::Yellow }),
                ))),
                hints,
            );
        }
    }
}
