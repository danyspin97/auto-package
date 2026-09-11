//! Interactive TUI dashboard - redesigned for better UX.
//!
//! Three-level hierarchy:
//!
//!   Dashboard     → Package list with session stats
//!   PackageView   → Full package details (log, attempts, spec)
//!   AttemptView   → Detailed attempt inspection (error, prompt, diff)
//!
//! Navigation:
//!   - ↑↓/jk       : Navigate lists
//!   - ←→/hl      : Switch panes/go back
//!   - Enter      : Drill down
//!   - Esc        : Go up/back
//!   - Tab        : Cycle panes
//!   - 1-4        : Switch tabs
//!   - b          : Build selected package
//!   - r          : Reload/refresh
//!   - /          : Search
//!   - ?          : Help
//!   - q or C-c   : Quit

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use ap_build::agent::Usage;
use ap_ecosystem::pip::Node;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph,
    Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs, Wrap,
};
use ratatui::Terminal;

// ── Constants & Theme ─────────────────────────────────────────────────────────

const C_PRIMARY: Color = Color::Cyan;
const C_SUCCESS: Color = Color::Green;
const C_ERROR: Color = Color::Red;
const C_WARNING: Color = Color::Yellow;
const C_MUTED: Color = Color::DarkGray;
const C_TEXT: Color = Color::White;
const C_SELECT_BG: Color = Color::Rgb(35, 35, 55);
const C_BORDER_FOCUS: Color = Color::Cyan;
const C_BORDER: Color = Color::DarkGray;

// ── Animated Indicator ────────────────────────────────────────────────────────

fn build_icon() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
    if ms < 500 { "●" } else { "○" }
}

// ── Data Model ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum PkgStatus {
    Built,
    Failed,
    Building,
    WaitingDeps,
    NotStarted,
}

impl PkgStatus {
    fn icon(&self) -> &'static str {
        match self {
            PkgStatus::Built => "✓",
            PkgStatus::Failed => "✗",
            PkgStatus::Building => "◐",
            PkgStatus::WaitingDeps => "⏳",
            PkgStatus::NotStarted => "·",
        }
    }

    fn color(&self) -> Color {
        match self {
            PkgStatus::Built => C_SUCCESS,
            PkgStatus::Failed => C_ERROR,
            PkgStatus::Building => C_WARNING,
            PkgStatus::WaitingDeps => C_WARNING,
            PkgStatus::NotStarted => C_MUTED,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            PkgStatus::Built => "built",
            PkgStatus::Failed => "failed",
            PkgStatus::Building => "building",
            PkgStatus::WaitingDeps => "waiting",
            PkgStatus::NotStarted => "pending",
        }
    }
}

#[derive(Debug, Clone)]
struct Attempt {
    number: usize,
    kind: String,
    section: String,
    escalation: String,
    reasoning: String,
    error: String,
    prompt: String,
    diff: String,
    response: String,
    confidence: String,
    action: String,
    status: String, // "in_progress", "completed", "failed"
    phase: String,  // "starting", "calling_agent", "validating", "done"
}

#[derive(Debug)]
struct Package {
    name: String,
    rpm_name: String,
    version: String,
    status: PkgStatus,
    deps: Vec<String>,
    attempts: Vec<Attempt>,
    build_log: Vec<String>,
}

// ── Disk Loading ──────────────────────────────────────────────────────────────

fn load_attempts(fixes_dir: &Path, pkg_name: &str) -> Vec<Attempt> {
    let pkg_dir = fixes_dir.join(pkg_name.to_lowercase());
    let read = |dir: &Path, f: &str| fs::read_to_string(dir.join(f)).unwrap_or_default();
    let mut attempts: Vec<Attempt> = Vec::new();

    // Helper to read status from JSON if available, fallback to text files
    let read_status = |dir: &Path| -> (String, String) {
        let status_json_path = dir.join("status.json");
        if status_json_path.exists() {
            if let Ok(json) = fs::read_to_string(&status_json_path) {
                if let Ok(status) = serde_json::from_str::<serde_json::Value>(&json) {
                    let state = status["state"].as_str().unwrap_or("in_progress").to_string();
                    let phase = status["phase"].as_str().unwrap_or("starting").to_string();
                    return (state, phase);
                }
            }
        }
        // Fallback to status.txt
        let status_txt = read(dir, "status.txt");
        let status = if status_txt.is_empty() { "in_progress".to_string() } else { status_txt };
        (status, "done".to_string())
    };

    // Review slot
    let review_dir = pkg_dir.join("review");
    if review_dir.is_dir() {
        let (status, phase) = read_status(&review_dir);
        attempts.push(Attempt {
            number: 0,
            kind: "review".to_string(),
            section: String::new(),
            escalation: String::new(),
            reasoning: read(&review_dir, "reasoning.txt"),
            error: read(&review_dir, "error.txt"),
            prompt: read(&review_dir, "prompt.txt"),
            diff: read(&review_dir, "diff.txt"),
            response: read(&review_dir, "response.txt"),
            confidence: read(&review_dir, "confidence.txt"),
            action: read(&review_dir, "action.txt"),
            status,
            phase,
        });
    }

    // Numbered attempts
    let mut numbered: Vec<(usize, PathBuf)> = fs::read_dir(&pkg_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if !path.is_dir() { return None; }
            let n: usize = e.file_name().to_str()?.parse().ok()?;
            Some((n, path))
        })
        .collect();
    numbered.sort_by_key(|(n, _)| *n);

    for (_, dir) in numbered {
        let (status, phase) = read_status(&dir);
        attempts.push(Attempt {
            number: 0,
            kind: read(&dir, "kind.txt"),
            section: read(&dir, "section.txt"),
            escalation: read(&dir, "escalation.txt"),
            reasoning: read(&dir, "reasoning.txt"),
            error: read(&dir, "error.txt"),
            prompt: read(&dir, "prompt.txt"),
            diff: read(&dir, "diff.txt"),
            response: read(&dir, "response.txt"),
            confidence: read(&dir, "confidence.txt"),
            action: read(&dir, "action.txt"),
            status,
            phase,
        });
    }

    for (i, a) in attempts.iter_mut().enumerate() {
        a.number = i + 1;
    }

    attempts
}

fn load_packages(work_dir: &Path, project: &str, fixes_dir: &Path) -> Vec<Package> {
    let cache_dir = work_dir.join("cache").join("resolve");
    let mut all_nodes: HashMap<String, Node> = HashMap::new();

    if let Ok(entries) = fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            if let Ok(json) = fs::read_to_string(entry.path()) {
                if let Ok(nodes) = serde_json::from_str::<Vec<Node>>(&json) {
                    for node in nodes {
                        let key = super::normalize(&node.name);
                        all_nodes.entry(key).or_insert(node);
                    }
                }
            }
        }
    }

    let mut names: HashSet<String> = HashSet::new();
    let proj_dir = work_dir.join(project);

    if let Ok(entries) = fs::read_dir(&proj_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(n) = entry.file_name().to_str().and_then(|s| s.strip_prefix("python-")) {
                    names.insert(super::normalize(n));
                }
            }
        }
    }

    if let Ok(entries) = fs::read_dir(fixes_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(n) = entry.file_name().to_str() {
                    names.insert(super::normalize(n));
                }
            }
        }
    }

    for key in all_nodes.keys() {
        names.insert(key.clone());
    }

    if names.is_empty() {
        return vec![];
    }

    let known_keys: HashSet<String> = names.iter().map(|n| super::normalize(n)).collect();
    let nodes: Vec<Node> = names
        .iter()
        .map(|name| {
            let key = super::normalize(name);
            all_nodes.get(&key).cloned().unwrap_or_else(|| Node {
                name: name.clone(),
                version: String::new(),
                sha256: String::new(),
                requires_dist: vec![],
                requires_python: None,
                is_requested: false,
                is_yanked: false,
            })
        })
        .collect();

    super::topo_sort(&nodes)
        .into_iter()
        .map(|i| {
            let node = &nodes[i];
            let rpm_name = ap_build::osc::rpm_name(&node.name);
            let pkg_dir = work_dir.join(project).join(&rpm_name);
            let status = if super::already_built(&pkg_dir, &node.version) {
                PkgStatus::Built
            } else if fixes_dir.join(node.name.to_lowercase()).exists() {
                PkgStatus::Failed
            } else {
                PkgStatus::NotStarted
            };
            let deps: Vec<String> = node
                .requires_dist
                .iter()
                .filter_map(|req| super::dep_name(req))
                .map(|d| super::normalize(d))
                .filter(|d| known_keys.contains(d))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            Package {
                name: node.name.clone(),
                rpm_name,
                version: node.version.clone(),
                status,
                deps,
                attempts: load_attempts(fixes_dir, &node.name),
                build_log: Vec::new(),
            }
        })
        .collect()
}

// ── Background Build ──────────────────────────────────────────────────────────

enum BuildEvent {
    Log(String),
    Done(Option<Usage>),
}

struct BuildConfig {
    work_dir: PathBuf,
    fixes_dir: PathBuf,
    project: String,
    distro: String,
    arch: String,
    allow_full_log: bool,
}

fn start_background_build(
    pkg_name: &str,
    cfg: &BuildConfig,
    total_usage: Arc<Mutex<Usage>>,
) -> mpsc::Receiver<BuildEvent> {
    let (tx, rx) = mpsc::channel::<BuildEvent>();
    let pkg = pkg_name.to_string();
    let work_dir = cfg.work_dir.clone();
    let fixes_dir = cfg.fixes_dir.clone();
    let project = cfg.project.clone();
    let distro = cfg.distro.clone();
    let arch = cfg.arch.clone();
    let allow_full_log = cfg.allow_full_log;
    let tx_agent = tx.clone();
    let tx_log = tx.clone();

    std::thread::spawn(move || {
        ap_build::agent::set_agent_logger(move |s| {
            tx_agent.send(BuildEvent::Log(s.to_owned())).ok();
        });

        let log = Arc::new(move |s: &str| {
            tx_log.send(BuildEvent::Log(s.to_owned())).ok();
        }) as super::Logger;

        let result = super::cmd_build(
            &pkg, &work_dir, &fixes_dir, &project, &distro, &arch,
            allow_full_log, false, "chroot", false, 1, log, total_usage,
        );
        tx.send(BuildEvent::Done(result.ok())).ok();
    });
    rx
}

// ── Log Routing ───────────────────────────────────────────────────────────────

fn strip_pkg_prefix(line: &str) -> &str {
    let trimmed = line.trim_start_matches('\n').trim_start();
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some(close) = rest.find(']') {
            return rest[close + 1..].trim_start();
        }
    }
    line.trim_start_matches('\n')
}

fn pkg_from_log_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches('\n').trim_start();
    let rest = trimmed.strip_prefix('[')?;
    let close = rest.find(']')?;
    let inner = &rest[..close];
    let name = inner.split_whitespace().next()?.trim();
    if name.is_empty() { None } else { Some(name) }
}

// ── View States ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Dashboard,
    Package,
    Attempt,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackageTab {
    Log = 0,
    Attempts = 1,
    Spec = 2,
}

impl PackageTab {
    fn label(self) -> &'static str {
        match self {
            PackageTab::Log => "Log",
            PackageTab::Attempts => "Attempts",
            PackageTab::Spec => "Spec",
        }
    }

    fn from_index(i: usize) -> Self {
        match i {
            0 => PackageTab::Log,
            1 => PackageTab::Attempts,
            _ => PackageTab::Spec,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttemptTab {
    Error = 0,
    Prompt = 1,
    Response = 2,
    Diff = 3,
}

impl AttemptTab {
    fn label(self) -> &'static str {
        match self {
            AttemptTab::Error => "Error",
            AttemptTab::Prompt => "Prompt",
            AttemptTab::Response => "Response",
            AttemptTab::Diff => "Diff",
        }
    }

    fn from_index(i: usize) -> Self {
        match i {
            0 => AttemptTab::Error,
            1 => AttemptTab::Prompt,
            2 => AttemptTab::Response,
            _ => AttemptTab::Diff,
        }
    }
}

// ── App State ─────────────────────────────────────────────────────────────────

struct App {
    // Data
    packages: Vec<Package>,
    session_log: Vec<String>,
    last_active_pkg: Option<String>,

    // View state
    view: View,
    selected_pkg_idx: Option<usize>,
    selected_attempt_idx: Option<usize>,

    // Lists
    pkg_list_state: ListState,
    attempt_list_state: ListState,

    // Tabs
    pkg_tab: PackageTab,
    attempt_tab: AttemptTab,

    // Scroll
    pkg_scroll: usize,
    attempt_scroll: usize,

    // Search
    search_mode: bool,
    search_query: String,
    search_results: Vec<usize>,
    search_anchor: usize,

    // Build
    build_rx: Option<mpsc::Receiver<BuildEvent>>,
    building_pkg: Option<String>,
    session_usage: Arc<Mutex<Usage>>,

    // Config
    build_cfg: BuildConfig,
    work_dir: PathBuf,
    fixes_dir: PathBuf,

    // UI state
    show_help: bool,
    status_msg: Option<String>,
    last_reload: std::time::Instant,
}

impl App {
    fn new(
        packages: Vec<Package>,
        build_cfg: BuildConfig,
        work_dir: PathBuf,
        fixes_dir: PathBuf,
    ) -> Self {
        let has_packages = !packages.is_empty();
        let mut pkg_list_state = ListState::default();
        if has_packages {
            pkg_list_state.select(Some(0));
        }

        Self {
            packages,
            session_log: Vec::new(),
            last_active_pkg: None,
            view: View::Dashboard,
            selected_pkg_idx: if has_packages { Some(0) } else { None },
            selected_attempt_idx: None,
            pkg_list_state,
            attempt_list_state: ListState::default(),
            pkg_tab: PackageTab::Log,
            attempt_tab: AttemptTab::Error,
            pkg_scroll: 0,
            attempt_scroll: 0,
            search_mode: false,
            search_query: String::new(),
            search_results: Vec::new(),
            search_anchor: 0,
            build_rx: None,
            building_pkg: None,
            session_usage: Arc::new(Mutex::new(Usage::default())),
            build_cfg,
            work_dir,
            fixes_dir,
            show_help: false,
            status_msg: None,
            last_reload: std::time::Instant::now(),
        }
    }

    fn is_building(&self) -> bool {
        self.build_rx.is_some()
    }

    fn selected_pkg(&self) -> Option<&Package> {
        let idx = self.selected_pkg_idx?;
        if self.search_query.is_empty() {
            self.packages.get(idx)
        } else {
            self.search_results.get(idx).and_then(|&i| self.packages.get(i))
        }
    }

    fn selected_pkg_mut(&mut self) -> Option<&mut Package> {
        let idx = self.selected_pkg_idx?;
        if self.search_query.is_empty() {
            self.packages.get_mut(idx)
        } else {
            let real_idx = *self.search_results.get(idx)?;
            self.packages.get_mut(real_idx)
        }
    }

    fn selected_attempt(&self) -> Option<&Attempt> {
        let pkg = self.selected_pkg()?;
        self.selected_attempt_idx.and_then(|i| pkg.attempts.get(i))
    }

    fn visible_count(&self) -> usize {
        if self.search_query.is_empty() {
            self.packages.len()
        } else {
            self.search_results.len()
        }
    }

    fn update_search(&mut self) {
        if self.search_query.is_empty() {
            self.search_results.clear();
            let n = self.packages.len();
            if n > 0 {
                self.selected_pkg_idx = Some(self.search_anchor.min(n - 1));
                self.pkg_list_state.select(Some(self.search_anchor.min(n - 1)));
            }
            return;
        }

        let q = self.search_query.to_lowercase();
        self.search_results = self
            .packages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.name.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();

        if self.search_results.is_empty() {
            self.selected_pkg_idx = None;
            self.pkg_list_state.select(None);
            return;
        }

        let sel = self.search_results
            .iter()
            .position(|&full_idx| full_idx >= self.search_anchor)
            .unwrap_or(0);
        self.selected_pkg_idx = Some(sel);
        self.pkg_list_state.select(Some(sel));
    }

    fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_results.clear();
        self.search_mode = false;
        let n = self.packages.len();
        if n > 0 {
            self.selected_pkg_idx = Some(self.search_anchor.min(n - 1));
            self.pkg_list_state.select(Some(self.search_anchor.min(n - 1)));
        }
    }

    fn route_line(&mut self, line: String) {
        if let Some(name) = pkg_from_log_line(&line) {
            let norm = super::normalize(name);
            if let Some(pkg) = self.packages.iter_mut().find(|p| super::normalize(&p.name) == norm) {
                self.last_active_pkg = Some(pkg.name.clone());
                let stripped = strip_pkg_prefix(&line);
                pkg.build_log.push(stripped.to_owned());

                // Update status if building
                if self.building_pkg.as_deref() == Some(&pkg.name) {
                    pkg.status = PkgStatus::Building;
                }
                return;
            }
        }

        if let Some(ref last) = self.last_active_pkg.clone() {
            let norm = super::normalize(last);
            if let Some(pkg) = self.packages.iter_mut().find(|p| super::normalize(&p.name) == norm) {
                pkg.build_log.push(line);
                return;
            }
        }

        self.session_log.push(line);
    }

    fn poll_build(&mut self) {
        let Some(rx) = &self.build_rx else { return };
        let mut lines = Vec::new();
        let mut done: Option<Option<Usage>> = None;

        loop {
            match rx.try_recv() {
                Ok(BuildEvent::Log(l)) => lines.push(l),
                Ok(BuildEvent::Done(u)) => { done = Some(u); break; }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => { done = Some(None); break; }
            }
        }

        for l in lines {
            self.route_line(l);
        }

        // Reload more frequently during builds to pick up new attempts immediately
        if self.build_rx.is_some() && self.last_reload.elapsed() >= Duration::from_millis(500) {
            self.merge_packages();
            self.last_reload = std::time::Instant::now();
        }

        if let Some(usage) = done {
            if let Some(u) = usage {
                self.session_usage.lock().unwrap().add(&u);
            }
            self.building_pkg = None;
            self.build_rx = None;
            self.reload();
            let cost = self.session_usage.lock().unwrap().cost_usd();
            self.status_msg = Some(format!("Build finished — session cost ~${cost:.4}"));
        }
    }

    fn merge_packages(&mut self) {
        let fresh = load_packages(&self.work_dir, &self.build_cfg.project, &self.fixes_dir);
        let existing_norms: HashSet<String> = self.packages.iter()
            .map(|p| super::normalize(&p.name))
            .collect();

        for pkg in &mut self.packages {
            let norm = super::normalize(&pkg.name);
            if let Some(f) = fresh.iter().find(|fp| super::normalize(&fp.name) == norm) {
                pkg.status = f.status.clone();
                pkg.version = f.version.clone();
                pkg.attempts = f.attempts.clone();
            }
        }

        for f in fresh {
            if !existing_norms.contains(&super::normalize(&f.name)) {
                self.packages.push(f);
            }
        }

        self.apply_waiting_status();
    }

    fn apply_waiting_status(&mut self) {
        let waiting: Vec<usize> = self.packages.iter().enumerate()
            .filter(|(_, p)| {
                matches!(p.status, PkgStatus::Failed | PkgStatus::NotStarted)
                    && p.build_log.iter().any(|l| l.contains("deferring until built"))
            })
            .map(|(i, _)| i)
            .collect();

        for i in waiting {
            self.packages[i].status = PkgStatus::WaitingDeps;
        }
    }

    fn reload(&mut self) {
        let prev_name = self.selected_pkg().map(|p| p.name.clone());

        let saved_logs: HashMap<String, Vec<String>> = self.packages.iter()
            .filter(|p| !p.build_log.is_empty())
            .map(|p| (super::normalize(&p.name), p.build_log.clone()))
            .collect();

        self.packages = load_packages(&self.work_dir, &self.build_cfg.project, &self.fixes_dir);

        for pkg in &mut self.packages {
            let norm = super::normalize(&pkg.name);
            if let Some(log) = saved_logs.get(&norm) {
                pkg.build_log = log.clone();
            }
        }

        if let Some(building) = self.building_pkg.clone() {
            let norm = super::normalize(&building);
            if !self.packages.iter().any(|p| super::normalize(&p.name) == norm) {
                self.packages.insert(0, Package {
                    name: building.clone(),
                    rpm_name: ap_build::osc::rpm_name(&building),
                    version: String::new(),
                    status: PkgStatus::Building,
                    deps: vec![],
                    attempts: vec![],
                    build_log: saved_logs.get(&norm).cloned().unwrap_or_default(),
                });
            }
        }

        let new_idx = prev_name
            .as_deref()
            .and_then(|name| self.packages.iter().position(|p| p.name == name))
            .or(self.selected_pkg_idx);

        if let Some(idx) = new_idx {
            if idx < self.packages.len() {
                self.selected_pkg_idx = Some(idx);
                self.pkg_list_state.select(Some(idx));
            }
        }

        self.apply_waiting_status();
    }

    fn start_build(&mut self, pkg_name: &str) {
        if self.is_building() {
            self.status_msg = Some("A build is already running".into());
            return;
        }

        self.last_active_pkg = None;
        self.building_pkg = Some(pkg_name.to_string());

        if !self.packages.iter().any(|p| super::normalize(&p.name) == super::normalize(pkg_name)) {
            self.packages.insert(0, Package {
                name: pkg_name.to_string(),
                rpm_name: ap_build::osc::rpm_name(pkg_name),
                version: String::new(),
                status: PkgStatus::Building,
                deps: vec![],
                attempts: vec![],
                build_log: vec![],
            });
        }

        if let Some(idx) = self.packages.iter().position(|p| {
            super::normalize(&p.name) == super::normalize(pkg_name)
        }) {
            self.selected_pkg_idx = Some(idx);
            self.pkg_list_state.select(Some(idx));
        }

        let rx = start_background_build(
            pkg_name, &self.build_cfg, Arc::clone(&self.session_usage),
        );
        self.build_rx = Some(rx);
        self.last_reload = std::time::Instant::now();
    }

    // ── Input Handlers ────────────────────────────────────────────────────────

    fn on_key(&mut self, key: KeyCode, mods: KeyModifiers) -> bool {
        // Global quit
        if matches!(key, KeyCode::Char('q')) || (matches!(key, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL)) {
            return false;
        }

        if self.show_help {
            self.show_help = false;
            return true;
        }

        // Search mode
        if self.search_mode {
            match key {
                KeyCode::Esc | KeyCode::Enter => {
                    // Exit search mode but keep the filter active
                    self.search_mode = false;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.update_search();
                }
                KeyCode::Char(c) if c == 'u' && mods.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+U clears the search input
                    self.search_query.clear();
                    self.update_search();
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.update_search();
                }
                _ => {}
            }
            return true;
        }

        // Global keys
        match key {
            KeyCode::Char('?') => { self.show_help = true; return true; }
            KeyCode::Char('r') => { self.reload(); return true; }
            _ => {}
        }

        match self.view {
            View::Dashboard => self.handle_dashboard_key(key),
            View::Package => self.handle_package_key(key),
            View::Attempt => self.handle_attempt_key(key),
        }

        true
    }

    fn handle_dashboard_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                // Clear search filter if active
                if !self.search_query.is_empty() {
                    self.clear_search();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_pkg_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_pkg_selection(1),
            KeyCode::Char('g') => self.move_pkg_selection(-(self.packages.len() as i32)),
            KeyCode::Char('G') => self.move_pkg_selection(self.packages.len() as i32),
            KeyCode::PageUp => self.move_pkg_selection(-10),
            KeyCode::PageDown => self.move_pkg_selection(10),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if self.selected_pkg().is_some() {
                    self.view = View::Package;
                    self.pkg_scroll = usize::MAX; // Auto-scroll to bottom of log

                    // Initialize attempt list
                    if let Some(pkg) = self.selected_pkg() {
                        if !pkg.attempts.is_empty() {
                            self.selected_attempt_idx = Some(0);
                            self.attempt_list_state.select(Some(0));
                        } else {
                            self.selected_attempt_idx = None;
                            self.attempt_list_state.select(None);
                        }
                    }
                }
            }
            KeyCode::Char('b') => {
                if let Some(name) = self.selected_pkg().map(|p| p.name.clone()) {
                    self.start_build(&name);
                }
            }
            KeyCode::Char('/') => {
                self.search_anchor = self.selected_pkg_idx.unwrap_or(0);
                self.search_mode = true;
            }
            _ => {}
        }
    }

    fn handle_package_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                self.view = View::Dashboard;
                self.pkg_scroll = 0;
            }
            KeyCode::Char('1') => { self.pkg_tab = PackageTab::Log; self.pkg_scroll = 0; }
            KeyCode::Char('2') => { self.pkg_tab = PackageTab::Attempts; self.pkg_scroll = 0; }
            KeyCode::Char('3') => { self.pkg_tab = PackageTab::Spec; self.pkg_scroll = 0; }
            KeyCode::Tab => {
                self.pkg_tab = PackageTab::from_index((self.pkg_tab as usize + 1) % 3);
                self.pkg_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.pkg_tab == PackageTab::Attempts {
                    self.move_attempt_selection(-1);
                } else {
                    self.pkg_scroll = self.pkg_scroll.saturating_sub(3);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.pkg_tab == PackageTab::Attempts {
                    self.move_attempt_selection(1);
                } else {
                    self.pkg_scroll = self.pkg_scroll.saturating_add(3);
                }
            }
            KeyCode::PageUp => self.pkg_scroll = self.pkg_scroll.saturating_sub(20),
            KeyCode::PageDown => self.pkg_scroll = self.pkg_scroll.saturating_add(20),
            KeyCode::Char('g') => self.pkg_scroll = 0,
            KeyCode::Char('G') => self.pkg_scroll = usize::MAX,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if self.pkg_tab == PackageTab::Attempts && self.selected_attempt().is_some() {
                    self.view = View::Attempt;
                    self.attempt_scroll = 0;
                }
            }
            KeyCode::Char('b') => {
                if let Some(name) = self.selected_pkg().map(|p| p.name.clone()) {
                    self.start_build(&name);
                }
            }
            _ => {}
        }
    }

    fn handle_attempt_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                self.view = View::Package;
                self.attempt_scroll = 0;
            }
            KeyCode::Char('1') => { self.attempt_tab = AttemptTab::Error; self.attempt_scroll = 0; }
            KeyCode::Char('2') => { self.attempt_tab = AttemptTab::Prompt; self.attempt_scroll = 0; }
            KeyCode::Char('3') => { self.attempt_tab = AttemptTab::Response; self.attempt_scroll = 0; }
            KeyCode::Char('4') => { self.attempt_tab = AttemptTab::Diff; self.attempt_scroll = 0; }
            KeyCode::Tab => {
                self.attempt_tab = AttemptTab::from_index((self.attempt_tab as usize + 1) % 4);
                self.attempt_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.attempt_scroll = self.attempt_scroll.saturating_sub(3);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.attempt_scroll = self.attempt_scroll.saturating_add(3);
            }
            KeyCode::PageUp => self.attempt_scroll = self.attempt_scroll.saturating_sub(20),
            KeyCode::PageDown => self.attempt_scroll = self.attempt_scroll.saturating_add(20),
            KeyCode::Char('g') => self.attempt_scroll = 0,
            KeyCode::Char('G') => self.attempt_scroll = usize::MAX,
            _ => {}
        }
    }

    fn move_pkg_selection(&mut self, delta: i32) {
        let n = self.visible_count();
        if n == 0 { return; }
        let cur = self.selected_pkg_idx.unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        self.selected_pkg_idx = Some(next);
        self.pkg_list_state.select(Some(next));
    }

    fn move_attempt_selection(&mut self, delta: i32) {
        let n = self.selected_pkg().map(|p| p.attempts.len()).unwrap_or(0);
        if n == 0 { return; }
        let cur = self.selected_attempt_idx.unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        self.selected_attempt_idx = Some(next);
        self.attempt_list_state.select(Some(next));
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();

    // Main layout: title bar + content + status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);

    render_title_bar(f, app, chunks[0]);

    match app.view {
        View::Dashboard => render_dashboard(f, app, chunks[1]),
        View::Package => render_package(f, app, chunks[1]),
        View::Attempt => render_attempt(f, app, chunks[1]),
    }

    render_status_bar(f, app, chunks[2]);

    if app.show_help {
        render_help(f);
    }
}

fn render_title_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let usage = app.session_usage.lock().unwrap().clone();
    let cost = usage.cost_usd();

    let building_icon = if app.is_building() {
        format!(" {} ", build_icon())
    } else {
        String::new()
    };

    let breadcrumb = match app.view {
        View::Dashboard => String::new(),
        View::Package => {
            let pkg = app.selected_pkg().map(|p| p.name.as_str()).unwrap_or("?");
            format!(" › {}", pkg)
        }
        View::Attempt => {
            let pkg = app.selected_pkg().map(|p| p.name.as_str()).unwrap_or("?");
            let att = app.selected_attempt().map(|a| format!("#{}", a.number)).unwrap_or_default();
            format!(" › {} › {}", pkg, att)
        }
    };

    let title = Line::from(vec![
        Span::styled(" auto-package", Style::default().fg(C_PRIMARY).bold()),
        Span::styled(building_icon, Style::default().fg(C_WARNING).bold()),
        Span::styled(breadcrumb, Style::default().fg(C_TEXT)),
    ]);

    let cost_str = format!("${:.4} ", cost);
    let cost_text = Line::from(Span::styled(cost_str, Style::default().fg(C_MUTED)));

    f.render_widget(Paragraph::new(title), area);
    f.render_widget(
        Paragraph::new(cost_text).alignment(Alignment::Right),
        area,
    );
}

fn render_dashboard(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let chunks = if app.is_building() {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(area)
    };

    render_package_list(f, app, chunks[0]);

    if app.is_building() && chunks.len() > 1 {
        render_live_log(f, app, chunks[1]);
    }
}

fn render_package_list(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let title = if !app.search_query.is_empty() {
        let n = app.search_results.len();
        format!(" Packages — /{} ({} match{}) ",
            app.search_query, n, if n == 1 { "" } else { "es" })
    } else {
        format!(" Packages ({}) ", app.packages.len())
    };

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER_FOCUS))
        .title(title)
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);

    // Reserve space for search bar if active
    let (list_area, search_area) = if app.search_mode || !app.search_query.is_empty() {
        let la = Rect { height: inner.height.saturating_sub(1), ..inner };
        let sa = Rect { y: inner.y + inner.height.saturating_sub(1), height: 1, ..inner };
        (la, Some(sa))
    } else {
        (inner, None)
    };

    f.render_widget(block, area);

    let active_norm = app.last_active_pkg.as_deref()
        .or(app.building_pkg.as_deref())
        .map(super::normalize);

    let make_item = |pkg: &Package| -> ListItem {
        let is_building = app.is_building()
            && active_norm.as_deref() == Some(super::normalize(&pkg.name).as_str());

        let (icon, color) = if is_building {
            (build_icon(), C_WARNING)
        } else {
            (pkg.status.icon(), pkg.status.color())
        };

        let attempts_tag = if pkg.attempts.is_empty() {
            String::new()
        } else {
            format!(" [{}]", pkg.attempts.len())
        };

        let version = if pkg.version.is_empty() {
            String::new()
        } else {
            format!(" {}", pkg.version)
        };

        ListItem::new(Line::from(vec![
            Span::styled(format!("{} ", icon), Style::default().fg(color).bold()),
            Span::styled(pkg.name.clone(), Style::default().fg(C_TEXT)),
            Span::styled(version, Style::default().fg(C_MUTED)),
            Span::styled(attempts_tag, Style::default().fg(C_PRIMARY)),
        ]))
    };

    let items: Vec<ListItem> = if app.search_query.is_empty() {
        app.packages.iter().map(make_item).collect()
    } else {
        app.search_results.iter().map(|&i| make_item(&app.packages[i])).collect()
    };

    let list = List::new(items)
        .highlight_style(Style::default().bg(C_SELECT_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, list_area, &mut app.pkg_list_state);

    if let Some(sa) = search_area {
        let cursor = if app.search_mode { "█" } else { "" };
        let query_display = format!("/{}{}", app.search_query, cursor);
        f.render_widget(
            Paragraph::new(query_display).style(Style::default().fg(C_PRIMARY)),
            sa,
        );
    }
}

fn render_live_log(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let active = app.last_active_pkg.as_deref()
        .or(app.building_pkg.as_deref())
        .unwrap_or("?");

    let title = format!(" {} {} ", build_icon(), active);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_WARNING))
        .title(title)
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let active_norm = app.last_active_pkg.as_deref()
        .or(app.building_pkg.as_deref())
        .map(super::normalize);

    let lines: Vec<Line> = app.packages.iter()
        .find(|p| active_norm.as_deref() == Some(super::normalize(&p.name).as_str()))
        .map(|p| {
            p.build_log.iter().map(|l| color_log_line(l)).collect()
        })
        .unwrap_or_default();

    let total = lines.len();
    let visible = inner.height as usize;
    let scroll = total.saturating_sub(visible);

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll as u16, 0)),
        inner,
    );
}

fn render_package(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    // Clone the necessary package data to avoid borrow issues
    let (pkg_name, pkg_status, build_log, attempts, rpm_name) = {
        let Some(pkg) = app.selected_pkg() else {
            f.render_widget(
                Paragraph::new("No package selected")
                    .style(Style::default().fg(C_MUTED)),
                area,
            );
            return;
        };
        (pkg.name.clone(), pkg.status.clone(), pkg.build_log.clone(), pkg.attempts.clone(), pkg.rpm_name.clone())
    };

    let title = format!(" {} — {} ", pkg_name, pkg_status.label());
    let tabs = vec![
        PackageTab::Log.label(),
        PackageTab::Attempts.label(),
        PackageTab::Spec.label(),
    ];

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER_FOCUS))
        .title(title)
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);

    // Tab bar
    let tab_area = Rect { height: 1, ..inner };
    let content_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };

    f.render_widget(block, area);

    let tabs_widget = Tabs::new(tabs)
        .select(app.pkg_tab as usize)
        .style(Style::default().fg(C_MUTED))
        .highlight_style(Style::default().fg(C_PRIMARY).bold().underlined())
        .divider(" │ ");

    f.render_widget(tabs_widget, tab_area);

    match app.pkg_tab {
        PackageTab::Log => render_package_log(f, app, &build_log, content_area),
        PackageTab::Attempts => render_attempts_list(f, app, &attempts, content_area),
        PackageTab::Spec => render_package_spec(f, app, &rpm_name, content_area),
    }
}

fn render_package_log(f: &mut ratatui::Frame, app: &mut App, build_log: &[String], area: Rect) {
    let lines: Vec<Line> = build_log.iter()
        .map(|l| color_log_line(l))
        .collect();

    let total = lines.len();
    let visible = area.height as usize;

    // Clamp scroll
    if app.pkg_scroll == usize::MAX || app.pkg_scroll > total.saturating_sub(visible) {
        app.pkg_scroll = total.saturating_sub(visible);
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.pkg_scroll as u16, 0)),
        area,
    );

    draw_scrollbar(f, area, total, visible, app.pkg_scroll);
}

fn render_attempts_list(f: &mut ratatui::Frame, app: &mut App, attempts: &[Attempt], area: Rect) {
    if attempts.is_empty() {
        f.render_widget(
            Paragraph::new("No attempts yet")
                .style(Style::default().fg(C_MUTED)),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = attempts.iter().map(|a| {
        let (badge, badge_color) = match a.kind.as_str() {
            "review" => ("R", Color::Magenta),
            "rpmspec" => ("S", C_PRIMARY),
            "test" => ("T", C_WARNING),
            _ => ("B", C_ERROR),
        };

        // Status indicator with phase
        let (status_icon, status_color) = match a.status.as_str() {
            "in_progress" => {
                match a.phase.as_str() {
                    "calling_agent" => ("⋯", C_PRIMARY),  // Waiting for Claude
                    "validating" => ("◐", C_WARNING),      // Validating
                    _ => ("●", C_WARNING),                 // Starting/other
                }
            }
            "failed" => ("✗", C_ERROR),
            "completed" | "" => ("✓", C_SUCCESS),
            _ => ("?", C_MUTED),
        };

        let reasoning = a.reasoning.lines().next().unwrap_or("").trim();
        let max = 55usize;
        let reasoning = if reasoning.len() > max {
            &reasoning[..max]
        } else {
            reasoning
        };

        ListItem::new(Line::from(vec![
            Span::styled(status_icon.to_string(), Style::default().fg(status_color).bold()),
            Span::raw(" "),
            Span::styled(format!("#{:<2} ", a.number), Style::default().fg(C_PRIMARY).bold()),
            Span::styled(format!("[{}]", badge), Style::default().fg(badge_color).bold()),
            Span::raw(" "),
            Span::styled(reasoning, Style::default().fg(C_TEXT)),
        ]))
    }).collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(C_SELECT_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.attempt_list_state);
}

fn render_package_spec(f: &mut ratatui::Frame, app: &mut App, rpm_name: &str, area: Rect) {
    let spec_path = app.work_dir.join(&app.build_cfg.project)
        .join(rpm_name)
        .join(format!("{}.spec", rpm_name));

    let content = fs::read_to_string(&spec_path)
        .unwrap_or_else(|_| "Spec file not found".to_string());

    let lines: Vec<Line> = content.lines()
        .map(|l| Line::from(l.to_owned()))
        .collect();

    let total = lines.len();
    let visible = area.height as usize;

    if app.pkg_scroll > total.saturating_sub(visible) {
        app.pkg_scroll = total.saturating_sub(visible);
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.pkg_scroll as u16, 0)),
        area,
    );

    draw_scrollbar(f, area, total, visible, app.pkg_scroll);
}

fn render_attempt(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    // Clone attempt data to avoid borrow issues
    let attempt = {
        let Some(attempt) = app.selected_attempt() else {
            f.render_widget(
                Paragraph::new("No attempt selected")
                    .style(Style::default().fg(C_MUTED)),
                area,
            );
            return;
        };
        attempt.clone()
    };

    let title = format!(" Attempt #{} — {} ", attempt.number, attempt.kind);
    let tabs = vec![
        AttemptTab::Error.label(),
        AttemptTab::Prompt.label(),
        AttemptTab::Diff.label(),
    ];

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER_FOCUS))
        .title(title)
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);

    // Tab bar + metadata line + content
    let tab_area = Rect { height: 1, ..inner };
    let meta_area = Rect { y: inner.y + 1, height: 1, ..inner };
    let content_area = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };

    f.render_widget(block, area);

    let tabs_widget = Tabs::new(tabs)
        .select(app.attempt_tab as usize)
        .style(Style::default().fg(C_MUTED))
        .highlight_style(Style::default().fg(C_PRIMARY).bold().underlined())
        .divider(" │ ");

    f.render_widget(tabs_widget, tab_area);

    // Metadata line
    let mut meta_spans = vec![];
    if !attempt.confidence.is_empty() {
        meta_spans.push(Span::styled(
            format!("{} confidence", attempt.confidence),
            Style::default().fg(C_PRIMARY).bold()
        ));
    }
    if !attempt.action.is_empty() {
        if !meta_spans.is_empty() {
            meta_spans.push(Span::styled(" · ", Style::default().fg(C_MUTED)));
        }
        meta_spans.push(Span::styled(attempt.action.clone(), Style::default().fg(C_SUCCESS)));
    }
    if !attempt.section.is_empty() {
        if !meta_spans.is_empty() {
            meta_spans.push(Span::styled(" · ", Style::default().fg(C_MUTED)));
        }
        meta_spans.push(Span::styled(attempt.section.clone(), Style::default().fg(C_TEXT)));
    }
    if !attempt.escalation.is_empty() {
        if !meta_spans.is_empty() {
            meta_spans.push(Span::styled(" · ", Style::default().fg(C_MUTED)));
        }
        meta_spans.push(Span::styled(attempt.escalation.clone(), Style::default().fg(C_MUTED)));
    }
    if !attempt.reasoning.is_empty() {
        if !meta_spans.is_empty() {
            meta_spans.push(Span::styled(" · ", Style::default().fg(C_MUTED)));
        }
        let reasoning_short = attempt.reasoning.lines().next().unwrap_or("");
        meta_spans.push(Span::styled(reasoning_short.to_owned(), Style::default().fg(C_TEXT)));
    }

    f.render_widget(Paragraph::new(Line::from(meta_spans)), meta_area);

    // Content
    let text = match app.attempt_tab {
        AttemptTab::Error => {
            if attempt.error.is_empty() {
                attempt.reasoning.clone()
            } else {
                attempt.error.clone()
            }
        }
        AttemptTab::Prompt => attempt.prompt.clone(),
        AttemptTab::Response => {
            if attempt.response.is_empty() {
                // Fallback to old format
                let mut r = String::new();
                if !attempt.confidence.is_empty() {
                    r.push_str(&format!("Confidence: {}\n", attempt.confidence));
                }
                if !attempt.action.is_empty() {
                    r.push_str(&format!("Action: {}\n", attempt.action));
                }
                r.push_str(&format!("Reasoning: {}\n", attempt.reasoning));
                r
            } else {
                attempt.response.clone()
            }
        }
        AttemptTab::Diff => attempt.diff.clone(),
    };

    let lines = render_text_lines(&text, app.attempt_tab);
    let total = lines.len();
    let visible = content_area.height as usize;

    if app.attempt_scroll > total.saturating_sub(visible) {
        app.attempt_scroll = total.saturating_sub(visible);
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.attempt_scroll as u16, 0)),
        content_area,
    );

    draw_scrollbar(f, content_area, total, visible, app.attempt_scroll);
}

fn render_text_lines(text: &str, tab: AttemptTab) -> Vec<Line<'static>> {
    if tab == AttemptTab::Diff {
        text.lines().map(|l| {
            if l.starts_with("@@") {
                Line::from(Span::styled(l.to_owned(), Style::default().fg(C_PRIMARY)))
            } else if l.starts_with('+') {
                Line::from(Span::styled(l.to_owned(), Style::default().fg(C_SUCCESS).bold()))
            } else if l.starts_with('-') {
                Line::from(Span::styled(l.to_owned(), Style::default().fg(C_ERROR)))
            } else {
                Line::from(l.to_owned())
            }
        }).collect()
    } else {
        text.lines().map(|l| Line::from(l.to_owned())).collect()
    }
}

fn render_status_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let msg_area = Rect { height: 1, ..inner };

    if let Some(msg) = &app.status_msg {
        f.render_widget(
            Paragraph::new(msg.as_str()).style(Style::default().fg(C_WARNING)),
            msg_area,
        );
        return;
    }

    let keys: &[(&str, &str)] = match app.view {
        View::Dashboard => &[
            ("↑↓/jk", "move"),
            ("Enter/→", "open"),
            ("/", "search"),
            ("b", "build"),
            ("r", "reload"),
            ("?", "help"),
            ("q", "quit"),
        ],
        View::Package => &[
            ("1-3", "tabs"),
            ("↑↓/jk", "scroll/move"),
            ("Enter/→", "open"),
            ("←/Esc", "back"),
            ("b", "build"),
            ("?", "help"),
        ],
        View::Attempt => &[
            ("1-4", "tabs"),
            ("↑↓/jk", "scroll"),
            ("←/Esc", "back"),
            ("?", "help"),
        ],
    };

    let mut spans: Vec<Span> = Vec::new();
    for (i, (k, d)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(*k, Style::default().fg(C_PRIMARY).bold()));
        spans.push(Span::styled(format!(" {}", d), Style::default().fg(C_MUTED)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), msg_area);
}

fn render_help(f: &mut ratatui::Frame) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_BORDER_FOCUS))
        .title(" Help ")
        .padding(Padding::horizontal(2));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled("Navigation", Style::default().fg(C_PRIMARY).bold())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ↑↓ / j k      ", Style::default().fg(C_TEXT)),
            Span::styled("Move up/down", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  ←→ / h l      ", Style::default().fg(C_TEXT)),
            Span::styled("Go back/forward", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  Enter         ", Style::default().fg(C_TEXT)),
            Span::styled("Open/drill down", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  Esc           ", Style::default().fg(C_TEXT)),
            Span::styled("Go back/up", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  Tab           ", Style::default().fg(C_TEXT)),
            Span::styled("Cycle tabs", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  1-4           ", Style::default().fg(C_TEXT)),
            Span::styled("Switch to tab", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  g / G         ", Style::default().fg(C_TEXT)),
            Span::styled("Jump to top/bottom", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  PgUp / PgDn   ", Style::default().fg(C_TEXT)),
            Span::styled("Page up/down", Style::default().fg(C_MUTED)),
        ]),
        Line::from(""),
        Line::from(Span::styled("Actions", Style::default().fg(C_PRIMARY).bold())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  b             ", Style::default().fg(C_TEXT)),
            Span::styled("Build selected package", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  r             ", Style::default().fg(C_TEXT)),
            Span::styled("Reload/refresh", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  /             ", Style::default().fg(C_TEXT)),
            Span::styled("Search packages (Esc to clear)", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  ?             ", Style::default().fg(C_TEXT)),
            Span::styled("Toggle this help", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  q / Ctrl+C    ", Style::default().fg(C_TEXT)),
            Span::styled("Quit", Style::default().fg(C_MUTED)),
        ]),
        Line::from(""),
        Line::from(Span::styled("Views", Style::default().fg(C_PRIMARY).bold())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Dashboard     ", Style::default().fg(C_TEXT)),
            Span::styled("Package list & live log", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  Package       ", Style::default().fg(C_TEXT)),
            Span::styled("Log, attempts, spec", Style::default().fg(C_MUTED)),
        ]),
        Line::from(vec![
            Span::styled("  Attempt       ", Style::default().fg(C_TEXT)),
            Span::styled("Error, prompt, response, diff", Style::default().fg(C_MUTED)),
        ]),
        Line::from(""),
        Line::from(Span::styled("Press any key to close", Style::default().fg(C_MUTED).italic())),
    ];

    f.render_widget(
        Paragraph::new(Text::from(lines)).alignment(Alignment::Left),
        inner,
    );
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn centered_rect(percent_w: u16, percent_h: u16, area: Rect) -> Rect {
    let vt = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_h) / 2),
            Constraint::Percentage(percent_h),
            Constraint::Percentage((100 - percent_h) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_w) / 2),
            Constraint::Percentage(percent_w),
            Constraint::Percentage((100 - percent_w) / 2),
        ])
        .split(vt[1])[1]
}

fn color_log_line(l: &str) -> Line<'static> {
    if l.contains("✓") {
        Line::from(Span::styled(l.to_owned(), Style::default().fg(C_SUCCESS)))
    } else if l.contains("✗") {
        Line::from(Span::styled(l.to_owned(), Style::default().fg(C_ERROR)))
    } else if l.contains("⋯") || l.contains("●") {
        Line::from(Span::styled(l.to_owned(), Style::default().fg(C_WARNING)))
    } else {
        Line::from(l.to_owned())
    }
}

fn draw_scrollbar(f: &mut ratatui::Frame, area: Rect, total: usize, visible: usize, pos: usize) {
    if total <= visible {
        return;
    }
    let max = total.saturating_sub(visible);
    let mut st = ScrollbarState::new(max + 1).position(pos.min(max));
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area,
        &mut st,
    );
}

// ── Entry Point ───────────────────────────────────────────────────────────────

pub fn run(
    work_dir: &Path,
    fixes_dir: &Path,
    project: &str,
    distro: &str,
    arch: &str,
    allow_full_log: bool,
    auto_build: Option<&str>,
) -> io::Result<()> {
    let build_cfg = BuildConfig {
        work_dir: work_dir.to_path_buf(),
        fixes_dir: fixes_dir.to_path_buf(),
        project: project.to_string(),
        distro: distro.to_string(),
        arch: arch.to_string(),
        allow_full_log,
    };

    let packages = load_packages(work_dir, project, fixes_dir);
    let mut app = App::new(
        packages,
        build_cfg,
        work_dir.to_path_buf(),
        fixes_dir.to_path_buf(),
    );

    if let Some(pkg_name) = auto_build {
        app.start_build(pkg_name);
    }

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| render(f, &mut app))?;
        app.poll_build();

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.status_msg.is_some()
                    && !matches!(key.code, KeyCode::Char('b') | KeyCode::Char('?'))
                {
                    app.status_msg = None;
                }
                if !app.on_key(key.code, key.modifiers) {
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
