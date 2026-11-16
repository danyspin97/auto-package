use super::*;

/// Reads the explanation of a row (blocking: run on a thread).
pub type ExplainLoader = Arc<dyn Fn(&str) -> Result<Explanation, String> + Send + Sync>;

static EXPLAIN_DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// Tell the TUI which per-project work directory the run writes to, so the
/// `e` key can explain a package from its files.
pub fn set_explain_dir(dir: std::path::PathBuf) {
    *EXPLAIN_DIR.lock().unwrap_or_else(|p| p.into_inner()) = Some(dir);
}

/// The loader the `e` key uses: `ap_build::explain` over the work directory
/// given to [`set_explain_dir`], or `None` before one is known.
pub fn explain_loader() -> Option<ExplainLoader> {
    let dir = EXPLAIN_DIR
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    Some(Arc::new(move |row: &str| {
        // The notes file is named by the package's spelling on disk (see
        // `explain_cmd`), which may differ from the row's.
        let notes_file = ap_build::package_notes::path_for(row).and_then(|p| {
            ap_build::explain::package_notes_name(p.parent()?, row)
                .ok()
                .flatten()
        });
        let notes = notes_file
            .as_deref()
            .map(ap_build::package_notes::read)
            .unwrap_or_default();
        explain::explain(&explain::Query {
            project_dir: &dir,
            package: row,
            run: None,
            package_notes: notes,
            notes_file,
        })
    }))
}

pub enum ExplainLoad {
    Loading,
    Failed(String),
    Ready(Box<Explanation>),
}

/// The `e` key's scrollable explanation of one package.
pub struct ExplainView {
    pub row: String,
    pub load: ExplainLoad,
    pub scroll: Cell<usize>,
    pub started: Instant,
    pub(super) seq: u64,
    pub(super) rendered: RefCell<Option<(usize, Vec<explain::Line>)>>,
    width: Cell<usize>,
    viewport: Cell<usize>,
}

impl ExplainView {
    pub(super) fn new(row: &str, seq: u64) -> Self {
        Self {
            row: row.to_string(),
            load: ExplainLoad::Loading,
            scroll: Cell::new(0),
            started: Instant::now(),
            seq,
            rendered: RefCell::new(None),
            width: Cell::new(80),
            viewport: Cell::new(20),
        }
    }

    /// The report wrapped to `width`, rendered once per width.
    fn lines(&self, width: usize) -> Vec<explain::Line> {
        let ExplainLoad::Ready(ex) = &self.load else {
            return Vec::new();
        };
        let mut cache = self.rendered.borrow_mut();
        if cache.as_ref().is_none_or(|(w, _)| *w != width) {
            *cache = Some((width, explain::render(ex, width)));
        }
        cache.as_ref().map(|(_, l)| l.clone()).unwrap_or_default()
    }

    pub(super) fn total(&self) -> usize {
        self.lines(self.width.get()).len()
    }

    fn goto(&self, line: usize) {
        self.scroll
            .set(line.min(max_offset(self.total(), self.viewport.get())));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExplainAction {
    None,
    Close,
}

pub fn explain_key(v: &ExplainView, code: KeyCode) -> ExplainAction {
    let page = v.viewport.get().saturating_sub(1).max(1);
    let cur = v.scroll.get();
    match code {
        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('e') => return ExplainAction::Close,
        KeyCode::Down | KeyCode::Char('j') => v.goto(cur + 1),
        KeyCode::Up | KeyCode::Char('k') => v.goto(cur.saturating_sub(1)),
        KeyCode::PageDown | KeyCode::Char(' ') => v.goto(cur + page),
        KeyCode::PageUp => v.goto(cur.saturating_sub(page)),
        KeyCode::Home | KeyCode::Char('g') => v.goto(0),
        KeyCode::End | KeyCode::Char('G') => v.goto(usize::MAX),
        _ => {}
    }
    ExplainAction::None
}

/// Open the explanation of `row`, read on a thread.
pub fn open_explain(
    browse: &mut BrowseState,
    row: &str,
    loader: Option<&ExplainLoader>,
    jobs: &Sender<Job>,
) {
    let seq = browse.next_seq();
    let mut view = ExplainView::new(row, seq);
    let Some(loader) = loader else {
        view.load = ExplainLoad::Failed(
            "The work directory of this run is not known, so there are no files to explain from. \
             Run `auto-package explain <package> --work-dir ...` instead."
                .to_string(),
        );
        browse.modal = Some(Modal::Explain(view));
        return;
    };
    browse.modal = Some(Modal::Explain(view));
    let (loader, jobs, row) = (loader.clone(), jobs.clone(), row.to_string());
    std::thread::spawn(move || {
        let result = loader(&row);
        let _ = jobs.send(Job::Explain { row, seq, result });
    });
}

fn explain_style(kind: explain::Kind) -> Style {
    match kind {
        explain::Kind::Plain => Style::default(),
        explain::Kind::Heading => Style::default().add_modifier(Modifier::BOLD),
        explain::Kind::Label => Style::default().fg(Color::Cyan),
        explain::Kind::Good => Style::default().fg(Color::Green),
        explain::Kind::Bad => Style::default().fg(Color::Red),
        explain::Kind::Warn => Style::default().fg(Color::Yellow),
        explain::Kind::Dim => Style::default().fg(Color::DarkGray),
    }
}

pub(super) fn render_explain(f: &mut Frame, v: &ExplainView, area: Rect) {
    f.render_widget(Clear, area);
    let block = pane(&format!("Explain: {}", v.row), true);
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
    v.width.set(width);
    v.viewport.set(body.height as usize);
    match &v.load {
        ExplainLoad::Loading => {
            f.render_widget(
                Paragraph::new(format!(
                    "{} reading the run files\u{2026}",
                    spinner(v.started)
                ))
                .style(Style::default().fg(Color::Yellow)),
                body,
            );
            f.render_widget(Paragraph::new(hint_line("q/Esc close", width)), hints);
        }
        ExplainLoad::Failed(e) => {
            f.render_widget(
                Paragraph::new(e.clone())
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(Color::Red)),
                body,
            );
            f.render_widget(Paragraph::new(hint_line("q/Esc close", width)), hints);
        }
        ExplainLoad::Ready(_) => {
            let lines = v.lines(width);
            let scroll = v
                .scroll
                .get()
                .min(max_offset(lines.len(), body.height as usize));
            v.scroll.set(scroll);
            let shown: Vec<Line<'static>> = lines
                .iter()
                .skip(scroll)
                .take(body.height as usize)
                .map(|l| {
                    Line::from(
                        l.iter()
                            .map(|s| Span::styled(s.text.clone(), explain_style(s.kind)))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(shown), body);
            let pos = format!(
                "[{}-{}/{}]  ",
                scroll + 1,
                (scroll + body.height as usize).min(lines.len()),
                lines.len()
            );
            f.render_widget(
                Paragraph::new(hint_line(
                    &format!("{pos}j/k PgUp/PgDn g/G scroll \u{2502} q close"),
                    width,
                )),
                hints,
            );
        }
    }
}
