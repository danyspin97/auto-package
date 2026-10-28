//! The full-screen diff viewer and the submit confirmation of the TUI.
//!
//! Both are modals owned by the event loop (`BrowseState::modal`). The slow
//! parts (`osc rdiff`, the checks before a submit request, the request itself)
//! run on background threads behind a [`PackageBackend`] and come back as
//! [`Job`]s, so the UI stays responsive. Every decision is a function of
//! plain state, so it can be tested with a fake backend and no terminal.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ratatui::widgets::Clear;

use super::*;
use crate::diffview::{self, Diff, Rendered};
use ap_build::explain::{self, Explanation};

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
        let notes = ap_build::package_notes::path_for(row)
            .map(|_| ap_build::package_notes::read(row))
            .unwrap_or_default();
        explain::explain(&explain::Query {
            project_dir: &dir,
            package: row,
            run: None,
            package_notes: notes,
        })
    }))
}

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

/// An answer from a background thread.
pub enum Job {
    Diff {
        row: String,
        /// Which request this answers, so a retry's older answer is dropped.
        seq: u64,
        result: Result<String, String>,
    },
    Explain {
        row: String,
        seq: u64,
        result: Result<Explanation, String>,
    },
    Preview(Vec<SubmitEntry>),
    Submitted {
        row: String,
        result: Result<String, SubmitFailure>,
    },
}

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
    seq: u64,
    rendered: RefCell<Option<(usize, Rendered)>>,
    width: Cell<usize>,
    viewport: Cell<usize>,
}

impl DiffView {
    fn new(row: &str, source: DiffSource, load: DiffLoad, has_recorded: bool, seq: u64) -> Self {
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

    fn with_rendered<T>(&self, width: usize, f: impl FnOnce(&Rendered) -> T) -> Option<T> {
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

    fn max_scroll(&self) -> usize {
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

pub enum Modal {
    Diff(DiffView),
    Submit(SubmitModal),
    Explain(ExplainView),
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
    seq: u64,
    rendered: RefCell<Option<(usize, Vec<explain::Line>)>>,
    width: Cell<usize>,
    viewport: Cell<usize>,
}

impl ExplainView {
    fn new(row: &str, seq: u64) -> Self {
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

    fn total(&self) -> usize {
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

/// Fold an answer into the state.
pub fn apply_job(state: &mut TuiState, browse: &mut BrowseState, job: Job) {
    match job {
        Job::Diff { row, seq, result } => {
            if let Some(Modal::Diff(v)) = &mut browse.modal
                && v.row == row
                && v.seq == seq
            {
                v.load = match result {
                    Ok(text) => DiffLoad::Ready(diffview::parse(&text)),
                    Err(e) => DiffLoad::Failed(e),
                };
                *v.rendered.borrow_mut() = None;
                v.scroll.set(0);
            }
        }
        Job::Explain { row, seq, result } => {
            if let Some(Modal::Explain(v)) = &mut browse.modal
                && v.row == row
                && v.seq == seq
            {
                v.load = match result {
                    Ok(ex) => ExplainLoad::Ready(Box::new(ex)),
                    Err(e) => ExplainLoad::Failed(e),
                };
                *v.rendered.borrow_mut() = None;
                v.scroll.set(0);
            }
        }
        Job::Preview(entries) => {
            for e in &entries {
                if let Some(id) = &e.existing
                    && let Some(p) = state.packages.get_mut(&e.row)
                {
                    p.sr = Some(SrMark::Existing(id.clone()));
                }
            }
            if let Some(Modal::Submit(m)) = &mut browse.modal
                && matches!(m.stage, SubmitStage::Loading(_))
            {
                m.stage = SubmitStage::Ready(entries);
            }
        }
        Job::Submitted { row, result } => {
            let (mark, level, message) = match result {
                Ok(id) => (
                    SrMark::Created(id.clone()),
                    LogLevel::Success,
                    format!("Submit request #{id} created"),
                ),
                Err(SubmitFailure::Existing(id)) => (
                    SrMark::Existing(id.clone()),
                    LogLevel::Warn,
                    format!("Not submitted: request #{id} is already open"),
                ),
                Err(SubmitFailure::Other(why)) => (
                    SrMark::Failed(why.clone()),
                    LogLevel::Error,
                    format!("Submit failed: {why}"),
                ),
            };
            if let Some(p) = state.packages.get_mut(&row) {
                p.sr = Some(mark);
                p.add_log(level, message.clone());
            }
            state.add_global_log(level, format!("{row}: {message}"));
        }
    }
}

fn spinner(since: Instant) -> &'static str {
    const FRAMES: [&str; 10] = [
        "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
        "\u{2827}", "\u{2807}", "\u{280f}",
    ];
    FRAMES[(since.elapsed().as_millis() / 100 % 10) as usize]
}

fn centered(area: Rect, w_pct: u16, h_pct: u16) -> Rect {
    let w = area.width * w_pct / 100;
    let h = area.height * h_pct / 100;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn render_modal(f: &mut Frame, state: &TuiState, modal: &Modal, area: Rect) {
    match modal {
        Modal::Diff(v) => render_diff(f, state, v, area),
        Modal::Submit(m) => render_submit(f, m, centered(area, 92, 86)),
        Modal::Explain(v) => render_explain(f, v, area),
    }
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

fn render_explain(f: &mut Frame, v: &ExplainView, area: Rect) {
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

fn hint_line(text: &str, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        truncate_with_ellipsis(text, width),
        Style::default().fg(Color::DarkGray),
    ))
}

fn render_diff(f: &mut Frame, state: &TuiState, v: &DiffView, area: Rect) {
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

fn render_submit(f: &mut Frame, m: &SubmitModal, area: Rect) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    const DIFF: &str = "--- foo.spec\n+++ foo.spec\n@@ -1,3 +1,3 @@ Name: foo\n Name: foo\n-Version: 1.0\n+Version: 2.0\n License: MIT\n@@ -20,2 +20,2 @@\n-a\n+b\n c\n";

    struct Fake {
        diff: Result<String, String>,
        preview: Result<SubmitPreview, SubmitFailure>,
        submit: Result<String, SubmitFailure>,
        submitted: Mutex<Vec<String>>,
    }

    impl Fake {
        fn ok() -> Arc<dyn PackageBackend> {
            Arc::new(Self {
                diff: Ok(DIFF.to_string()),
                preview: Ok(SubmitPreview {
                    summary: "home:me/foo -> Base/foo (1.0 -> 2.0)".into(),
                    source: "home:me/foo".into(),
                    target: "Base/foo".into(),
                    description: "Update foo to 2.0.\n\nChangelog:\n- Update to 2.0".into(),
                }),
                submit: Ok("123".into()),
                submitted: Mutex::new(Vec::new()),
            })
        }
    }

    impl PackageBackend for Fake {
        fn diff(&self, _: &str) -> Result<String, String> {
            self.diff.clone()
        }
        fn preview(&self, _: &str) -> Result<SubmitPreview, SubmitFailure> {
            self.preview.clone()
        }
        fn submit(&self, row: &str) -> Result<String, SubmitFailure> {
            self.submitted.lock().unwrap().push(row.to_string());
            self.submit.clone()
        }
    }

    fn state(status: PkgStatus, finished: bool) -> TuiState {
        let mut s = TuiState::new(RunScope::default());
        let p = s.pkg_mut("foo");
        p.status = status;
        p.finished = finished;
        s
    }

    fn screen(state: &TuiState, browse: &BrowseState, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        terminal
            .draw(|f| {
                render(f, state, &mut table_state, None, browse, None);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn next_job(rx: &Receiver<Job>) -> Job {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn the_diff_loads_in_the_background_and_renders() {
        let st = state(PkgState::new(String::new()).status, false);
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let backend = Fake::ok();
        open_diff(&st, &mut b, "foo", Some(&backend), &tx);
        assert!(screen(&st, &b, 100, 30).contains("reading the diff"));
        apply_job(
            &mut TuiState::new(RunScope::default()),
            &mut b,
            next_job(&rx),
        );
        let out = screen(&st, &b, 100, 30);
        assert!(out.contains("1 file changed, +2 \u{2212}2"), "{out}");
        assert!(
            out.contains("Version: 2.0") && out.contains("foo.spec"),
            "{out}"
        );
        assert!(out.contains("j/k"), "keys hint: {out}");
    }

    #[test]
    fn a_failed_diff_shows_the_error_and_can_be_retried_or_replaced() {
        let mut st = state(PkgStatus::Running, false);
        st.pkg_mut("foo")
            .spec_diffs
            .push("- Version: 1\n+ Version: 2\n".into());
        let backend: Arc<dyn PackageBackend> = Arc::new(Fake {
            diff: Err("osc rdiff failed: HTTP Error 502".into()),
            preview: Err(SubmitFailure::Other("x".into())),
            submit: Err(SubmitFailure::Other("x".into())),
            submitted: Mutex::new(Vec::new()),
        });
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        open_diff(&st, &mut b, "foo", Some(&backend), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        let out = screen(&st, &b, 100, 30);
        assert!(
            out.contains("HTTP Error 502") && out.contains("r retry"),
            "{out}"
        );
        assert!(out.contains("recorded by this run"), "{out}");
        let Some(Modal::Diff(v)) = &mut b.modal else {
            panic!()
        };
        assert_eq!(diff_key(v, KeyCode::Char('r')), DiffAction::Retry);
        assert_eq!(diff_key(v, KeyCode::Char('s')), DiffAction::Recorded);
        redo_diff(&st, &mut b, DiffAction::Recorded, Some(&backend), &tx);
        assert!(screen(&st, &b, 100, 30).contains("Version: 2"));
        // The older, failed request's answer is ignored once the view moved on.
        apply_job(
            &mut st,
            &mut b,
            Job::Diff {
                row: "foo".into(),
                seq: 1,
                result: Err("late".into()),
            },
        );
        assert!(!screen(&st, &b, 100, 30).contains("late"));
    }

    #[test]
    fn without_a_backend_the_recorded_spec_changes_are_shown_or_none_said() {
        let mut st = state(PkgStatus::Running, false);
        let (tx, _rx) = channel();
        let mut b = BrowseState::default();
        open_diff(&st, &mut b, "foo", None, &tx);
        assert!(screen(&st, &b, 100, 30).contains("No changes recorded yet"));
        st.pkg_mut("foo").spec_diffs.push("- a\n+ b\n".into());
        open_diff(&st, &mut b, "foo", None, &tx);
        let out = screen(&st, &b, 100, 30);
        assert!(
            out.contains("recorded by this run") && out.contains("1 file changed"),
            "{out}"
        );
    }

    fn fake_explanation() -> Explanation {
        let mut ex = Explanation {
            package: "foo".into(),
            ..Default::default()
        };
        ex.outcome.result = "failed".into();
        ex.outcome.detail = Some("gave up after 2 attempts".into());
        ex.unknown = (0..60).map(|i| format!("unknown thing {i}")).collect();
        ex
    }

    #[test]
    fn the_explanation_loads_in_the_background_scrolls_and_closes() {
        let mut st = state(PkgStatus::Running, false);
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let loader: ExplainLoader = Arc::new(|row| {
            assert_eq!(row, "foo");
            Ok(fake_explanation())
        });
        open_explain(&mut b, "foo", Some(&loader), &tx);
        assert!(screen(&st, &b, 100, 30).contains("reading the run files"));
        apply_job(&mut st, &mut b, next_job(&rx));
        let out = screen(&st, &b, 100, 30);
        assert!(
            out.contains("Explain: foo") && out.contains("gave up after 2 attempts"),
            "{out}"
        );
        assert!(out.contains("PgUp/PgDn"), "keys hint: {out}");
        let Some(Modal::Explain(v)) = &b.modal else {
            panic!()
        };
        assert_eq!(explain_key(v, KeyCode::Char('G')), ExplainAction::None);
        assert!(v.scroll.get() > 0);
        let out = screen(&st, &b, 100, 30);
        assert!(
            out.contains("unknown thing 59") && !out.contains("Package foo"),
            "{out}"
        );
        assert_eq!(explain_key(v, KeyCode::Home), ExplainAction::None);
        assert_eq!(v.scroll.get(), 0);
        assert_eq!(explain_key(v, KeyCode::Char('q')), ExplainAction::Close);
        assert_eq!(explain_key(v, KeyCode::Esc), ExplainAction::Close);
    }

    #[test]
    fn a_failed_explanation_shows_the_error_and_a_late_answer_is_ignored() {
        let mut st = state(PkgStatus::Running, false);
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let loader: ExplainLoader = Arc::new(|_| Err("nothing about foo in /w".into()));
        open_explain(&mut b, "foo", Some(&loader), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        assert!(screen(&st, &b, 100, 30).contains("nothing about foo in /w"));
        apply_job(
            &mut st,
            &mut b,
            Job::Explain {
                row: "foo".into(),
                seq: 999,
                result: Ok(fake_explanation()),
            },
        );
        assert!(!screen(&st, &b, 100, 30).contains("gave up"));
        // Without a loader the modal says why instead of waiting forever.
        open_explain(&mut b, "foo", None, &tx);
        assert!(screen(&st, &b, 100, 30).contains("work directory of this run is not known"));
    }

    #[test]
    fn an_empty_diff_says_there_are_no_changes() {
        let st = state(PkgStatus::Running, false);
        let mut b = BrowseState::default();
        b.modal = Some(Modal::Diff(DiffView::new(
            "foo",
            DiffSource::Origin,
            DiffLoad::Ready(diffview::parse("")),
            false,
            1,
        )));
        assert!(screen(&st, &b, 100, 30).contains("No changes: the package is identical"));
    }

    #[test]
    fn diff_keys_scroll_jump_search_and_close() {
        let mut text = String::from("--- f\n+++ f\n");
        for h in 0..3 {
            text.push_str(&format!("@@ -{0},40 +{0},40 @@\n", h * 100 + 1));
            for i in 0..40 {
                text.push_str(&format!(" line {h}-{i}\n"));
            }
        }
        let st = state(PkgStatus::Running, false);
        let mut b = BrowseState::default();
        b.modal = Some(Modal::Diff(DiffView::new(
            "foo",
            DiffSource::Origin,
            DiffLoad::Ready(diffview::parse(&text)),
            false,
            1,
        )));
        screen(&st, &b, 100, 30);
        let Some(Modal::Diff(v)) = &mut b.modal else {
            panic!()
        };
        diff_key(v, KeyCode::Char('j'));
        assert_eq!(v.scroll.get(), 1);
        diff_key(v, KeyCode::Char('k'));
        diff_key(v, KeyCode::Char('k'));
        assert_eq!(v.scroll.get(), 0);
        diff_key(v, KeyCode::Char(']'));
        assert_eq!(v.scroll.get(), 2);
        diff_key(v, KeyCode::Char('n'));
        assert_eq!(v.scroll.get(), 43);
        diff_key(v, KeyCode::Char('['));
        assert_eq!(v.scroll.get(), 2);
        diff_key(v, KeyCode::Char('G'));
        assert_eq!(v.scroll.get(), v.max_scroll());
        diff_key(v, KeyCode::Char('g'));
        diff_key(v, KeyCode::PageDown);
        assert_eq!(v.scroll.get(), v.viewport.get() - 1);
        diff_key(v, KeyCode::Char('g'));
        for c in ['/', '2', '-', '7', '\n'] {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            diff_key(v, code);
        }
        let first = v.scroll.get();
        assert!(first > 0);
        assert!(
            v.with_rendered(100, |r| r.plain[first].contains("2-7"))
                .unwrap()
        );
        assert_eq!(diff_key(v, KeyCode::Esc), DiffAction::None);
        assert!(v.search.is_none());
        assert_eq!(diff_key(v, KeyCode::Esc), DiffAction::Close);
        assert_eq!(diff_key(v, KeyCode::Char('q')), DiffAction::Close);
    }

    fn built() -> TuiState {
        state(PkgStatus::Succeeded, true)
    }

    #[test]
    fn a_package_that_did_not_build_is_never_offered() {
        for status in [
            PkgStatus::Failed,
            PkgStatus::Running,
            PkgStatus::Queued,
            PkgStatus::MissingDeps,
        ] {
            let mut st = state(status, true);
            let (tx, _rx) = channel();
            let mut b = BrowseState::default();
            assert!(submit_rows(&st, None, true).is_empty());
            open_submit(&mut st, &mut b, vec!["foo".into()], Some(&Fake::ok()), &tx);
            assert!(b.modal.is_none());
            assert!(
                st.last_global_log
                    .unwrap()
                    .message
                    .contains("not submitted")
            );
        }
    }

    #[test]
    fn submitting_shows_what_is_sent_then_makes_one_request_on_yes() {
        let mut st = built();
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let fake = Fake::ok();
        let rows = submit_rows(&st, Some("foo"), false);
        open_submit(&mut st, &mut b, rows, Some(&fake), &tx);
        assert!(screen(&st, &b, 110, 36).contains("checking foo"));
        apply_job(&mut st, &mut b, next_job(&rx));
        let out = screen(&st, &b, 110, 36);
        for want in [
            "home:me/foo (branch project)",
            "Base/foo (origin project)",
            "1 file changed, +2",
            "Update to 2.0",
            "y create 1 submit request",
        ] {
            assert!(out.contains(want), "missing {want:?}:\n{out}");
        }
        let Some(Modal::Submit(m)) = &mut b.modal else {
            panic!()
        };
        assert_eq!(submit_key(m, KeyCode::Char('x')), SubmitAction::None);
        assert_eq!(submit_key(m, KeyCode::Char('y')), SubmitAction::Confirm);
        confirm_submit(&mut st, &mut b, Some(&fake), &tx);
        assert!(b.modal.is_none());
        assert_eq!(st.packages["foo"].sr, Some(SrMark::Submitting));
        apply_job(&mut st, &mut b, next_job(&rx));
        assert_eq!(st.packages["foo"].sr, Some(SrMark::Created("123".into())));
        assert!(screen(&st, &b, 110, 36).contains("SR #123"));
        assert!(
            st.last_global_log
                .as_ref()
                .unwrap()
                .message
                .contains("#123")
        );

        // A second time asks again, with a warning, and `A` leaves it out.
        assert!(submit_rows(&st, None, true).is_empty());
        open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        let out = screen(&st, &b, 110, 36);
        assert!(
            out.contains("already created") && out.contains("second request"),
            "{out}"
        );
    }

    #[test]
    fn no_is_the_default_and_nothing_is_sent() {
        let mut st = built();
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let fake = Fake::ok();
        open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        let Some(Modal::Submit(m)) = &mut b.modal else {
            panic!()
        };
        assert_eq!(submit_key(m, KeyCode::Char('n')), SubmitAction::Close);
        assert_eq!(submit_key(m, KeyCode::Esc), SubmitAction::Close);
        assert_eq!(submit_key(m, KeyCode::Enter), SubmitAction::None);
        assert_eq!(st.packages["foo"].sr, None);
    }

    #[test]
    fn an_ineligible_or_already_open_package_cannot_be_confirmed() {
        let mut st = built();
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let fake: Arc<dyn PackageBackend> = Arc::new(Fake {
            diff: Err("no network".into()),
            preview: Err(SubmitFailure::Existing("77".into())),
            submit: Err(SubmitFailure::Other("x".into())),
            submitted: Mutex::new(Vec::new()),
        });
        open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        assert_eq!(st.packages["foo"].sr, Some(SrMark::Existing("77".into())));
        let out = screen(&st, &b, 110, 30);
        assert!(
            out.contains("request #77 is already open") && out.contains("any key closes"),
            "{out}"
        );
        let Some(Modal::Submit(m)) = &mut b.modal else {
            panic!()
        };
        assert_eq!(submit_key(m, KeyCode::Char('y')), SubmitAction::Close);
        b.modal = None;
        assert!(screen(&st, &b, 140, 30).contains("SR #77 (open)"));
    }

    #[test]
    fn a_failed_request_is_marked_and_logged() {
        let mut st = built();
        let (tx, rx) = channel();
        let mut b = BrowseState::default();
        let fake: Arc<dyn PackageBackend> = Arc::new(Fake {
            diff: Ok(DIFF.into()),
            preview: Fake::ok_preview(),
            submit: Err(SubmitFailure::Other("HTTP 403".into())),
            submitted: Mutex::new(Vec::new()),
        });
        open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        confirm_submit(&mut st, &mut b, Some(&fake), &tx);
        apply_job(&mut st, &mut b, next_job(&rx));
        assert_eq!(
            st.packages["foo"].sr,
            Some(SrMark::Failed("HTTP 403".into()))
        );
        assert!(screen(&st, &b, 140, 30).contains("submit failed: HTTP 403"));
        // A failed one is offered again by `A`.
        assert_eq!(submit_rows(&st, None, true), vec!["foo".to_string()]);
    }

    #[test]
    fn without_a_backend_the_submit_keys_say_so() {
        let mut st = built();
        let (tx, _rx) = channel();
        let mut b = BrowseState::default();
        open_submit(&mut st, &mut b, vec!["foo".into()], None, &tx);
        assert!(b.modal.is_none());
        assert!(
            st.last_global_log
                .unwrap()
                .message
                .contains("update-project")
        );
    }

    #[test]
    fn a_request_in_flight_is_not_started_twice() {
        let mut st = built();
        st.pkg_mut("foo").sr = Some(SrMark::Submitting);
        let (tx, _rx) = channel();
        let mut b = BrowseState::default();
        assert!(submit_rows(&st, None, true).is_empty());
        open_submit(&mut st, &mut b, vec!["foo".into()], Some(&Fake::ok()), &tx);
        assert!(b.modal.is_none());
    }

    impl Fake {
        fn ok_preview() -> Result<SubmitPreview, SubmitFailure> {
            Ok(SubmitPreview {
                summary: "s".into(),
                source: "a/foo".into(),
                target: "b/foo".into(),
                description: "d".into(),
            })
        }
    }
}
