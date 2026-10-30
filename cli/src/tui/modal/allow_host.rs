//! Allowing a host that a package was refused for, from the TUI.
//!
//! `h` opens a prompt for the selected package's refused hosts, while it has
//! any still to allow. A host is offered only when the tool really refused it
//! for this package (the refusal record, with who asked, not the failure text
//! alone) and the failure names it. The prompt offers `PROMPT_HOSTS` at a time
//! and says how many more wait. Nothing is allowed by one key: `Y` allows the
//! offered hosts for this run; `p`, then typing a host's exact name, allows
//! that host permanently. `Y` and `p` are refused when the prompt's lines do
//! not fit the space it drew, so no host is allowed unseen. Esc (or `n`, `q`)
//! cancels; `a` and `A` keep their other meanings.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

use ap_build::allowed_hosts::file::{self, Added};
use ap_build::allowed_hosts::{
    AllowOutcome, AllowedHosts, Asker, Origin, Via, normalize_user_host, printable,
    refused_hosts_in,
};
use ap_core::event::{Event, HostScope, HostSource};

use super::*;

/// The most hosts one prompt offers. The rest wait for the next batch.
pub const PROMPT_HOSTS: usize = 4;

/// A host the tool refused for a package, still not allowed, and named by the
/// package's failure, with each request that asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub host: String,
    pub askers: Vec<Asker>,
}

/// The hosts of `name`'s failure that the tool really refused for `name`, that
/// are still not allowed, and that a user may allow (see
/// [`normalize_user_host`]). A host refused for another package is not offered
/// here, even when this failure names it; a host in the failure text alone is
/// not offered either, since anything printed by a service can name one.
pub fn pending_hosts(name: &str, pkg: &PkgState, hosts: &AllowedHosts) -> Vec<Candidate> {
    let Some(reason) = pkg.fail_reason.as_deref() else {
        return Vec::new();
    };
    let named = refused_hosts_in(reason);
    let mut out: Vec<Candidate> = Vec::new();
    for (host, use_, asker) in hosts.refusals() {
        if asker.package != name || !named.contains(&host) || hosts.allows(&host, use_) {
            continue;
        }
        if normalize_user_host(&host).is_err() {
            continue;
        }
        match out.iter_mut().find(|c| c.host == host) {
            Some(c) => {
                if !c.askers.contains(&asker) {
                    c.askers.push(asker);
                }
            }
            None => out.push(Candidate {
                host,
                askers: vec![asker],
            }),
        }
    }
    out
}

/// The selected package and its pending hosts, when there are any.
pub fn allow_target(
    state: &TuiState,
    selected: Option<&str>,
    hosts: &AllowedHosts,
) -> Option<(String, Vec<Candidate>)> {
    let name = selected?;
    let pending = pending_hosts(name, state.packages.get(name)?, hosts);
    (!pending.is_empty()).then(|| (name.to_string(), pending))
}

/// Whether `r` should offer a retry of a package that was refused a host
/// which is now allowed. A package refused and not yet allowed is not
/// retried: the retry would be refused again.
pub fn retry_after_allow(name: &str, pkg: &PkgState, hosts: &AllowedHosts) -> bool {
    pkg.status == PkgStatus::Failed
        && pkg.fail_reason.as_deref().is_some_and(|r| {
            !refused_hosts_in(r).is_empty() && pending_hosts(name, pkg, hosts).is_empty()
        })
}

/// Why `r` may retry `pkg` now, for the footer and the log: no space left, or a
/// host it was refused that is now allowed. `None` when there is no retry.
pub fn retry_label(name: &str, pkg: &PkgState, hosts: &AllowedHosts) -> Option<&'static str> {
    if pkg.retry_requested {
        None
    } else if pkg.status == PkgStatus::NoSpace {
        Some("no space left on device")
    } else if retry_after_allow(name, pkg, hosts) {
        Some("host allowed")
    } else {
        None
    }
}

/// Allow one host (checked as a user name) with `origin`. `Ok(Some(host))`
/// when the list changed, `Ok(None)` when it already allowed the host.
fn allow_one(
    list: &AllowedHosts,
    raw: &str,
    origin: Origin,
    reason: Option<String>,
) -> Result<Option<String>, String> {
    let host = normalize_user_host(raw)?;
    match list.allow(&host, origin, reason)? {
        AllowOutcome::Added => Ok(Some(host)),
        AllowOutcome::AlreadyAllowed(_) => Ok(None),
    }
}

/// Allow `hosts` for this run, for `name`. Returns the hosts that were added:
/// a host already allowed is not reported as added, and an error is logged.
pub fn allow_for_run(
    state: &mut TuiState,
    name: &str,
    hosts: &[String],
    list: &AllowedHosts,
) -> Vec<String> {
    let mut added = Vec::new();
    for raw in hosts {
        match allow_one(list, raw, Origin::Run(Via::Tui), None) {
            Ok(Some(host)) => added.push(host),
            Ok(None) => {}
            Err(e) => state.add_global_log(LogLevel::Error, format!("{raw} is not allowed: {e}")),
        }
    }
    if !added.is_empty() {
        state.add_global_log(
            LogLevel::Info,
            format!("{name}: allowed {} for this run", added.join(", ")),
        );
        state.add_global_log(LogLevel::Info, format!("press r to retry {name}"));
    }
    added
}

/// Record each host the TUI allowed, through the run's reporter (if the run
/// has one yet).
pub fn record_allowed(
    recorder: &RecorderSlot,
    hosts: &[String],
    scope: HostScope,
    source: HostSource,
) {
    let reporter = recorder.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(reporter) = reporter {
        for host in hosts {
            reporter.report(&Event::HostAllowed {
                host: host.clone(),
                scope,
                source,
            });
        }
    }
}

/// Open the prompt for the selected package's pending hosts, when there are
/// any. Does nothing otherwise.
pub fn open_allow(
    state: &TuiState,
    modal: &mut Option<Modal>,
    selected: Option<&str>,
    hosts: &AllowedHosts,
) {
    if let Some((package, candidates)) = allow_target(state, selected, hosts) {
        *modal = Some(Modal::AllowHost(AllowHostModal::new(
            package,
            candidates,
            file::config_file(),
        )));
    }
}

/// The prompt for `package`: the hosts to allow, and where a permanent allow
/// is written (`None`: there is no config directory, so no permanent allow).
pub struct AllowHostModal {
    pub package: String,
    /// The hosts still to decide, in order. The first `batch` are offered now.
    pub hosts: Vec<Candidate>,
    /// How many of the first hosts are offered and not yet decided.
    batch: usize,
    /// The size of the offered batch when it was offered, for "k of n".
    batch_total: usize,
    path: Option<PathBuf>,
    /// The name being typed for `hosts[0]`, once `p` was pressed.
    typing: Option<String>,
    finished: bool,
    /// The body size (width, height) the last frame drew. `Y` and `p` need it:
    /// they are refused until a frame has drawn the prompt, and when the lines
    /// would not fit that body.
    body: Cell<Option<(u16, u16)>>,
}

impl AllowHostModal {
    pub fn new(package: String, hosts: Vec<Candidate>, path: Option<PathBuf>) -> Self {
        let mut m = Self {
            package,
            hosts,
            batch: 0,
            batch_total: 0,
            path,
            typing: None,
            finished: false,
            body: Cell::new(None),
        };
        m.next_batch();
        m
    }

    /// Offer the next batch of up to `PROMPT_HOSTS`, or finish when none is left.
    fn next_batch(&mut self) {
        self.typing = None;
        self.batch = self.hosts.len().min(PROMPT_HOSTS);
        self.batch_total = self.batch;
        self.finished = self.hosts.is_empty();
    }

    /// Whether the prompt has nothing left to do and should close.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Whether the lines the prompt shows fit the body the last frame drew.
    /// Before a frame has drawn it, they do not.
    pub fn fits(&self) -> bool {
        let Some((width, height)) = self.body.get() else {
            return false;
        };
        let rows: usize = allow_host_lines(self)
            .iter()
            .map(|line| wrapped_rows(line, usize::from(width)))
            .sum();
        rows <= usize::from(height)
    }

    /// Take `hosts[0]` out of the list, for a decision; the batch shrinks, and
    /// the next batch starts when this one is empty.
    fn decide_first(&mut self) -> Candidate {
        let decided = self.hosts.remove(0);
        self.batch -= 1;
        if self.batch == 0 {
            self.next_batch();
        }
        decided
    }
}

/// The rows `line` takes when the paragraph wraps it at `width` columns: a word
/// that does not fit on the row moves down, a word longer than a row is broken,
/// and leading spaces count.
fn wrapped_rows(line: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut rows = 1;
    let mut col = line.chars().take_while(|c| *c == ' ').count().min(width);
    for word in line.split_whitespace() {
        let mut len = word.chars().count();
        if col > 0 && col + 1 + len <= width {
            col += 1 + len;
            continue;
        }
        if col > 0 {
            rows += 1;
        }
        while len > width {
            rows += 1;
            len -= width;
        }
        col = len;
    }
    rows
}

#[derive(Debug, PartialEq, Eq)]
pub enum AllowHostAction {
    None,
    /// Esc, `n` or `q`: nothing is allowed.
    Close,
    /// `Y`: allow these hosts for this run.
    RunAll(Vec<String>),
    /// The typed name of one host: allow it permanently.
    Permanent(String),
    /// `Y` or a typed name, when the prompt would not fit the space it drew.
    TooSmall,
}

/// What a key does in the prompt. Only `Y`, `p` and the typed name of a host
/// allow anything; Enter and `y` do nothing at the choice step.
pub fn allow_host_key(m: &mut AllowHostModal, code: KeyCode) -> AllowHostAction {
    if m.typing.is_none() {
        return match code {
            KeyCode::Char('Y') => {
                if !m.fits() {
                    return AllowHostAction::TooSmall;
                }
                let hosts = m.hosts[..m.batch].iter().map(|c| c.host.clone()).collect();
                m.hosts.drain(..m.batch);
                m.next_batch();
                AllowHostAction::RunAll(hosts)
            }
            KeyCode::Char('p') if m.path.is_some() => {
                if !m.fits() {
                    return AllowHostAction::TooSmall;
                }
                m.typing = Some(String::new());
                AllowHostAction::None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => AllowHostAction::Close,
            _ => AllowHostAction::None,
        };
    }
    match code {
        KeyCode::Esc => AllowHostAction::Close,
        KeyCode::Backspace => {
            if let Some(buf) = m.typing.as_mut() {
                buf.pop();
            }
            AllowHostAction::None
        }
        KeyCode::Char(c) if !c.is_control() => {
            if let Some(buf) = m.typing.as_mut() {
                buf.push(c);
            }
            AllowHostAction::None
        }
        KeyCode::Enter => confirm_typed(m),
        _ => AllowHostAction::None,
    }
}

/// Enter at a typed name: the name must be `hosts[0]`'s exact name, and the
/// prompt must fit. Then that host is decided, and the next one is typed.
fn confirm_typed(m: &mut AllowHostModal) -> AllowHostAction {
    let (Some(buf), Some(first)) = (m.typing.as_ref(), m.hosts.first()) else {
        return AllowHostAction::None;
    };
    if *buf != first.host {
        return AllowHostAction::None;
    }
    if !m.fits() {
        return AllowHostAction::TooSmall;
    }
    let host = m.decide_first().host;
    if m.batch > 0 {
        m.typing = Some(String::new());
    }
    AllowHostAction::Permanent(host)
}

/// Carry out the action of a key: allow the hosts for this run, or write one
/// host to the user's file and allow it. Records what was really allowed.
pub fn apply_allow_action(
    state: &mut TuiState,
    m: &mut AllowHostModal,
    action: AllowHostAction,
    list: &AllowedHosts,
    recorder: &RecorderSlot,
) {
    match action {
        AllowHostAction::None => {}
        AllowHostAction::Close => m.finished = true,
        AllowHostAction::RunAll(hosts) => {
            let added = allow_for_run(state, &m.package, &hosts, list);
            record_allowed(recorder, &added, HostScope::Run, HostSource::Tui);
        }
        AllowHostAction::TooSmall => state.add_global_log(
            LogLevel::Error,
            "terminal too small: enlarge it to see every host before allowing it".to_string(),
        ),
        AllowHostAction::Permanent(host) => {
            let path = m.path.clone();
            if let Some(added) = allow_permanent(state, &m.package, &host, path.as_deref(), list) {
                record_allowed(recorder, &[added], HostScope::Persistent, HostSource::Tui);
            }
        }
    }
}

/// Allow `host` for good: written to the user's file at `path`, and allowed
/// in the list now. Returns the host when it was newly allowed or written.
fn allow_permanent(
    state: &mut TuiState,
    package: &str,
    host: &str,
    path: Option<&Path>,
    list: &AllowedHosts,
) -> Option<String> {
    let host = match normalize_user_host(host) {
        Ok(host) => host,
        Err(e) => {
            state.add_global_log(LogLevel::Error, format!("{host} is not allowed: {e}"));
            return None;
        }
    };
    let reason = format!("allowed from the TUI for {package}");
    let Some(path) = path else {
        state.add_global_log(
            LogLevel::Error,
            format!("{host}: there is no config directory, so it is not written"),
        );
        return None;
    };
    match file::add_host(path, &host, Some(&reason)) {
        Ok(added) => {
            // The file now holds the host; the list takes it from the file.
            let _ = list.allow(&host, Origin::File, Some(reason));
            let what = match added {
                Added::New => format!("allowed {host} permanently in {}", path.display()),
                Added::AlreadyPresent => {
                    format!("{host} was already in {}", path.display())
                }
            };
            state.add_global_log(LogLevel::Info, what);
            state.add_global_log(LogLevel::Info, format!("press r to retry {package}"));
            (added == Added::New).then_some(host)
        }
        Err(e) => {
            let _ = allow_one(list, &host, Origin::Run(Via::Tui), None);
            state.add_global_log(
                LogLevel::Error,
                format!("{host} is allowed for this run only: {e}"),
            );
            None
        }
    }
}

/// `r`: retry the selected package when [`retry_label`] offers one. The retry
/// goes through the run's add channel, as an `n` add would.
pub fn retry_key(
    state: &mut TuiState,
    add_tx: Option<&Sender<AddRequest>>,
    work: &AtomicUsize,
    selected: Option<&str>,
    hosts: &AllowedHosts,
) {
    let (Some(add_tx), Some(name)) = (add_tx, selected) else {
        return;
    };
    let Some(why) = state
        .packages
        .get(name)
        .and_then(|p| retry_label(name, p, hosts))
    else {
        return;
    };
    if request_build(add_tx, work, AddRequest::Retry(name.to_string())) {
        if let Some(p) = state.packages.get_mut(name) {
            p.retry_requested = true;
        }
        state.add_global_log(LogLevel::Info, format!("Retrying {name} ({why})"));
    } else {
        state.add_global_log(
            LogLevel::Error,
            format!("Cannot retry {name}: the run is no longer accepting packages"),
        );
    }
}

/// The text of the prompt, one fact per line, with control characters removed.
pub(crate) fn allow_host_lines(m: &AllowHostModal) -> Vec<String> {
    let mut lines = vec![format!(
        "{} was refused. Allowing a host is a security decision: it covers the",
        printable(&m.package)
    )];
    lines.push("host, its subdomains, services and source fetches of this tool.".to_string());
    for c in &m.hosts[..m.batch] {
        lines.push(format!("  {}", printable(&c.host)));
        let who: Vec<String> = c
            .askers
            .iter()
            .map(|a| format!("{}, {}", printable(&a.package), a.what))
            .collect();
        lines.push(format!("      refused for: {}", who.join("; ")));
    }
    let more = m.hosts.len() - m.batch;
    if more > 0 {
        let what = if more == 1 {
            "1 more host is".to_string()
        } else {
            format!("{more} more hosts are")
        };
        lines.push(format!(
            "{what} refused for {}: offered after these are decided",
            printable(&m.package)
        ));
    }
    lines.push(String::new());
    match (&m.typing, &m.path) {
        (Some(typed), _) => {
            let host = &m.hosts[0].host;
            lines.push(format!(
                "type {} to allow it permanently ({} of {}): {}",
                printable(host),
                m.batch_total - m.batch + 1,
                m.batch_total,
                printable(typed)
            ));
            lines.push("Enter confirms when it matches; Esc cancels.".to_string());
        }
        (None, Some(path)) => {
            lines.push(format!("Y: allow these {} for this run only", m.batch));
            lines.push(format!(
                "p: allow permanently; type the host's name to confirm (written to {})",
                printable(&path.display().to_string())
            ));
            lines.push("Esc: cancel".to_string());
        }
        (None, None) => {
            lines.push(format!("Y: allow these {} for this run only", m.batch));
            lines.push("p: not available, there is no config directory to write to".to_string());
            lines.push("Esc: cancel".to_string());
        }
    }
    lines
}

pub(crate) fn render_allow_host(f: &mut Frame, m: &AllowHostModal, area: Rect) {
    f.render_widget(Clear, area);
    let block = pane("Allow a host", true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let [body, hints] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .areas(inner);
    m.body.set(Some((body.width, body.height)));
    let lines: Vec<Line> = allow_host_lines(m)
        .into_iter()
        .map(|text| {
            // A host's name is the line indented by two spaces only.
            let style = if text.starts_with("  ") && !text.starts_with("      ") {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    let width = inner.width as usize;
    f.render_widget(Paragraph::new(hint_line("Esc cancel", width)), hints);
}

#[cfg(test)]
#[path = "allow_host_tests.rs"]
mod allow_host_tests;
