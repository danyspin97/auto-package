//! Offering the one repository a search found for a bare name, on a terminal.
//!
//! A search hit nothing vouches for is never used silently. When a person is
//! there to answer and the evidence is clear (one repository of exactly the
//! name, not archived, not a fork, with releases, and the agent did not pick
//! another), it is offered with `use <repository>? [y/N]`. A yes is recorded
//! in the work directory's `sources.toml`, so the next run does not ask.

use std::path::Path;

use ap_build::pipeline::{Candidate, CandidateSource};

/// What the process is attached to: whether there is somebody to ask.
#[derive(Clone, Copy)]
pub(crate) struct Terminal {
    pub(crate) stdin: bool,
    pub(crate) stderr: bool,
    /// The TUI owns the screen (`ap_core::sink::is_quiet`).
    pub(crate) tui: bool,
}

impl Terminal {
    pub(crate) fn current() -> Self {
        use std::io::IsTerminal;
        Terminal {
            stdin: std::io::stdin().is_terminal(),
            stderr: std::io::stderr().is_terminal(),
            tui: ap_core::sink::is_quiet(),
        }
    }

    pub(crate) fn can_ask(self) -> bool {
        self.stdin && self.stderr && !self.tui
    }
}

/// The candidate to offer, if any: there is a terminal to ask on, exactly one
/// candidate is named for the package, it is not archived and not a fork, it
/// has at least one release (unknown metadata is no evidence), and the agent
/// did not pick a different repository.
pub(crate) fn offerable<'a>(
    name: &str,
    candidates: &'a [Candidate],
    term: Terminal,
) -> Option<&'a Candidate> {
    if !term.can_ask() {
        return None;
    }
    let mut exact = candidates.iter().filter(|c| c.is_exact(name));
    let (one, rest) = (exact.next()?, exact.next());
    let agent_elsewhere = candidates
        .iter()
        .any(|c| c.source == CandidateSource::Agent && !std::ptr::eq(c, one));
    (rest.is_none()
        && !agent_elsewhere
        && one.archived == Some(false)
        && one.fork == Some(false)
        && one.releases.is_some_and(|n| n >= 1))
    .then_some(one)
}

/// `use github.com/o/r (222 stars, 30 releases)?`
pub(crate) fn question(c: &Candidate) -> String {
    let shown = c.url.split_once("://").map_or(c.url.as_str(), |(_, r)| r);
    let count = |n: Option<u64>, one: &str, many: &str| match n {
        Some(1) => Some(format!("1 {one}")),
        Some(n) => Some(format!("{n} {many}")),
        None => None,
    };
    let facts: Vec<String> = [
        count(c.stars, "star", "stars"),
        count(c.releases, "release", "releases"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if facts.is_empty() {
        format!("use {shown}?")
    } else {
        format!("use {shown} ({})?", facts.join(", "))
    }
}

/// Whether an answer is a yes: `y` or `yes` in any case. Anything else, and no
/// answer at all (end of input), is a no.
pub(crate) fn is_yes(answer: Option<&str>) -> bool {
    answer.is_some_and(|a| matches!(a.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Ask whether to use `c`. `ask` shows the prompt and returns the line read.
pub(crate) fn confirm(c: &Candidate, ask: &dyn Fn(&str) -> Option<String>) -> bool {
    is_yes(ask(&format!("{} [y/N] ", question(c))).as_deref())
}

/// The repository the source map records for `name`, when it is one: an entry
/// such as `wheel` (the policy for a wheel-only package) is not a URL.
pub(crate) fn remembered(source_urls: &ap_build::sources::SourceMap, name: &str) -> Option<String> {
    source_urls
        .get(name)
        .filter(|u| u.contains("://"))
        .map(str::to_string)
}

/// Record `name = url` in `<work_dir>/sources.toml`, keeping what it holds.
pub(crate) fn remember(work_dir: &Path, name: &str, url: &str) -> Result<(), String> {
    let mut map = ap_build::sources::SourceMap::load(work_dir)?;
    map.insert(name, url);
    map.save(work_dir)
}

/// Use `url` for `name` because the person chose it, and remember it.
pub(crate) fn adopt(work_dir: &Path, name: &str, url: &str) -> String {
    ap_core::diag!("using {url} (confirmed by you)");
    match remember(work_dir, name, url) {
        Ok(()) => ap_core::diag!("remembered in sources.toml"),
        Err(e) => ap_core::diag!("warning: not remembered: {e}"),
    }
    url.to_string()
}

/// The repository for a name PyPI does not carry and the search could not
/// verify: a verified one, or the one the person confirms or picks on the
/// terminal. Otherwise the refusal that lists what was found, or the plain
/// message when nothing was.
pub(crate) fn discover(name: &str, work_dir: &Path) -> color_eyre::Result<String> {
    let repos = ap_build::pipeline::find_source_repo_candidates(name, name);
    if let Some(url) = repos.verified.first() {
        return Ok(url.clone());
    }
    let mut found = ap_build::pipeline::candidate_list(&repos, name);
    if found.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            crate::unconfirmed::no_repository_message(name)
        ));
    }
    ap_build::pipeline::fetch_metadata(&mut found);
    let ask = |p: &str| crate::update_project::ask_line(p, &|_| {});
    let term = Terminal::current();
    let chosen = match offerable(name, &found, term) {
        Some(c) => confirm(c, &ask).then_some(c),
        None => {
            let show = |s: &str| eprintln!("{s}");
            let view = (
                ap_core::sink::terminal_width(crate::name_clash::stderr_colour()),
                crate::name_clash::stderr_colour(),
            );
            crate::unconfirmed_pick::pick(name, &found, term, view, &show, &ask)
        }
    };
    if let Some(c) = chosen {
        return Ok(adopt(work_dir, name, &c.url));
    }
    Err(crate::unconfirmed::Unconfirmed {
        name: name.to_string(),
        candidates: found,
    }
    .into())
}
