//! A bare name PyPI and the distributions do not know, for which the
//! repository search found candidates that nothing confirms. They are never
//! used unasked; the refusal lists them, and says what to run to pick one.

use ap_build::pipeline::Candidate;
use ap_core::sink::{Role, pad_to, paint};

/// The refusal for a name whose search found only unconfirmed repositories:
/// its Display is the plain table `build` prints (see `name_clash::error_text`),
/// and the TUI shows `tui_line`.
#[derive(Debug)]
pub(crate) struct Unconfirmed {
    pub(crate) name: String,
    pub(crate) candidates: Vec<Candidate>,
}

/// The refusal for a name with no PyPI release and no repository found at all.
pub(crate) fn no_repository_message(name: &str) -> String {
    format!(
        "{name} has no usable PyPI release and no discoverable public source \
         repository. Pass --url, --source-url, or a repository URL as the \
         positional argument to point it at the source."
    )
}

/// `https://github.com/o/r` as shown in a row: without the scheme.
fn shown(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

fn count(n: Option<u64>, one: &str, many: &str) -> String {
    match n {
        Some(1) => format!("1 {one}"),
        Some(n) => format!("{n} {many}"),
        None => String::new(),
    }
}

/// What is known of a candidate, as the columns of its row: stars, releases.
fn facts(c: &Candidate) -> (String, String) {
    let releases = if c.releases == Some(0) {
        "no releases".to_string()
    } else {
        count(c.releases, "release", "releases")
    };
    (count(c.stars, "star", "stars"), releases)
}

/// `archived` and `fork` (when known to be so), in yellow, each after two spaces.
fn flags(c: &Candidate, colour: bool) -> String {
    let mut out = String::new();
    for (on, word) in [(c.archived, "archived"), (c.fork, "fork")] {
        if on == Some(true) {
            out.push_str("  ");
            out.push_str(&paint(word, Role::Warning, colour));
        }
    }
    out
}

/// The table `build` prints: the candidates one per row, with the command to
/// build the first. With `colour` the name is bold, the command green and the
/// flags yellow; stripped of escapes it is the plain text. A row that does not
/// fit `width` puts its facts on a second line, so the URL stays whole.
pub(crate) fn table(name: &str, candidates: &[Candidate], width: usize, colour: bool) -> String {
    use unicode_width::UnicodeWidthStr;
    let rows: Vec<(&str, (String, String))> = candidates
        .iter()
        .map(|c| (shown(&c.url), facts(c)))
        .collect();
    let col =
        |f: &dyn Fn(&(&str, (String, String))) -> usize| rows.iter().map(f).max().unwrap_or(0);
    let url_w = col(&|r| r.0.width());
    let stars_w = col(&|r| r.1.0.width());
    let rel_w = col(&|r| r.1.1.width());
    let flag_w = candidates
        .iter()
        .map(|c| flags(c, false).width())
        .max()
        .unwrap_or(0);
    let one_line = 2 + url_w + 2 + stars_w + 2 + rel_w + flag_w <= width;
    let mut out = format!(
        "{} has no PyPI release and nothing in openSUSE; the search found these \
         repositories but nothing confirms them:\n",
        paint(&format!("`{name}`"), Role::Package, colour)
    );
    for (c, (url, (stars, rel))) in candidates.iter().zip(&rows) {
        let right = format!("{stars:>stars_w$}");
        let mut line = if one_line {
            format!("  {}  {right}  {}", pad_to(url, url_w), pad_to(rel, rel_w))
        } else {
            format!("  {url}\n    {right}  {}", pad_to(rel, rel_w))
        };
        let flagged = flags(c, colour);
        if flagged.is_empty() {
            line.truncate(line.trim_end().len());
        } else {
            line.push_str(&flagged);
        }
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(first) = candidates.first() {
        out.push_str(&format!(
            "Pick one and run: {}",
            paint(
                &format!("auto-package build {}", first.url),
                Role::Success,
                colour
            )
        ));
    }
    out
}

impl Unconfirmed {
    /// The table `build` prints, laid out to `width`.
    pub(crate) fn table(&self, width: usize, colour: bool) -> String {
        table(&self.name, &self.candidates, width, colour)
    }

    /// One plain line for the TUI's warning row: the candidates and the way on.
    pub(crate) fn tui_line(&self) -> String {
        let urls: Vec<&str> = self.candidates.iter().map(|c| c.url.as_str()).collect();
        format!(
            "`{}` has no PyPI release and nothing in openSUSE; the search found {} but nothing \
             confirms them. Type a repository URL, or run `auto-package build {}`",
            self.name,
            urls.join(", "),
            urls.first().copied().unwrap_or_default()
        )
    }
}

impl std::fmt::Display for Unconfirmed {
    /// The plain table, at the default width.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.table(100, false))
    }
}

impl std::error::Error for Unconfirmed {}

#[cfg(test)]
#[path = "unconfirmed_tests.rs"]
mod tests;
