//! The recap of an update that jumps several upstream releases: the notes of
//! each release on their own, newest first, each under a bullet of its own,
//! within the limits of [`super::caps`].

use std::cmp::Ordering;

use super::agent_recap;
use super::caps::{self, BULLET_CHARS, PER_RELEASE, SHRUNK, cut, is_security, rank};
use super::changelog;
use super::markdown::strip_comments;
use super::render;
use super::routine::is_routine;
use crate::osc::ReleaseNotes;
use crate::sources::RepoRelease;
use crate::version;

/// One release of the window as its source gives it: its version, where its
/// notes are read (a release page, or a changelog file in the sources) and
/// the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) version: String,
    pub(super) link: String,
    pub(super) body: String,
}

/// One release of the jump, as the recap lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Release {
    pub(super) version: String,
    /// Where its notes are read: a release page, or a changelog file.
    pub(super) link: String,
    /// Its bullets, routine ones left out, in the order the notes give them.
    pub(super) bullets: Vec<String>,
    /// Its notes when they are prose rather than bullets: they have to be
    /// condensed before they can be listed.
    pub(super) prose: Option<String>,
}

impl Release {
    pub(super) fn has_notes(&self) -> bool {
        !self.bullets.is_empty() || self.prose.is_some()
    }
}

/// The releases of a changelog `text` (of the file `file`) between `old` and
/// `new`, newest first.
pub(super) fn from_section(file: &str, text: &str, old: &str, new: &str) -> Vec<Found> {
    changelog::releases_of_section(text, old, new)
        .into_iter()
        .map(|(version, body)| Found {
            version,
            link: format!("{file} in the sources"),
            body,
        })
        .collect()
}

/// The releases of `releases` (forge releases of `repo_url`), newest first.
pub(super) fn from_releases(repo_url: &str, releases: &[RepoRelease]) -> Vec<Found> {
    let repo = ap_ecosystem::forge::parse(repo_url)
        .map(|f| f.repo_name)
        .unwrap_or_default();
    let mut found: Vec<Found> = releases
        .iter()
        .map(|r| Found {
            version: crate::sources::version_from_tag(&r.tag, &repo),
            link: r.url.clone(),
            body: r.body.clone(),
        })
        .collect();
    found.sort_by(|a, b| version::sort_cmp(&b.version, &a.version));
    found
}

/// The release of a found body: its bullets with routine ones left out, or
/// its prose when it has notes and no bullet at all.
pub(super) fn release_of(f: &Found) -> Release {
    let raw = render::bullets_of(&f.body);
    let has_notes = !strip_comments(&f.body).trim().is_empty();
    let prose = (raw.is_empty() && has_notes).then(|| strip_comments(&f.body));
    Release {
        version: f.version.clone(),
        link: f.link.clone(),
        bullets: raw.into_iter().filter(|b| !is_routine(b)).collect(),
        prose,
    }
}

/// The security lines of a release: its bullets that name a fix, or for prose
/// the lines that do.
fn security_of(r: &Release) -> Vec<String> {
    match &r.prose {
        Some(text) => text
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', '+', ' ']).trim())
            .filter(|l| is_security(l))
            .map(str::to_string)
            .collect(),
        None => r
            .bullets
            .iter()
            .filter(|b| is_security(b))
            .cloned()
            .collect(),
    }
}

/// Everything a release lists: its bullets in their order (breaking changes
/// first, then security fixes, then the rest), each cut to [`BULLET_CHARS`]
/// unless it is a security line. Prose lists its security lines and the link.
fn full(r: &Release) -> Vec<String> {
    let mut items: Vec<String> = match r.prose {
        None => r
            .bullets
            .iter()
            .map(|b| {
                if is_security(b) {
                    b.clone()
                } else {
                    cut(b, BULLET_CHARS)
                }
            })
            .collect(),
        Some(_) => security_of(r),
    };
    items.sort_by_key(|b| rank(b));
    if r.prose.is_some() {
        items.push(format!("see {}", r.link));
    }
    items
}

/// A release shrunk to its link: its security lines, then `see <link>`.
fn only_link(r: &Release) -> Vec<String> {
    let mut items = security_of(r);
    items.sort_by_key(|b| rank(b));
    items.push(format!("see {}", r.link));
    items
}

/// The first `n` of `items` in their order, every security line kept whatever
/// `n` is.
fn keep(items: &[String], n: usize) -> Vec<String> {
    let security = items.iter().filter(|i| is_security(i)).count();
    let mut room = n.saturating_sub(security);
    items
        .iter()
        .filter(|i| {
            if is_security(i) {
                true
            } else if room > 0 {
                room -= 1;
                true
            } else {
                false
            }
        })
        .cloned()
        .collect()
}

/// Whether the lists fit the limits: no release over [`PER_RELEASE`] lines and
/// all of them together within [`caps::TOTAL`].
fn fits(lists: &[Vec<String>]) -> bool {
    lists.iter().all(|l| l.len() <= caps::PER_RELEASE)
        && lists.iter().map(Vec::len).sum::<usize>() <= caps::TOTAL
}

/// The lists of `releases` (the update's own release first) as they are, when
/// every release has bullets and the lists fit the limits. `None` otherwise:
/// prose needs condensing, and too many lines need shrinking.
pub(super) fn fitting(releases: &[Release]) -> Option<Vec<Vec<String>>> {
    if releases.iter().any(|r| r.prose.is_some()) {
        return None;
    }
    let lists: Vec<Vec<String>> = releases.iter().map(full).collect();
    fits(&lists).then_some(lists)
}

/// The lists of `releases` shrunk to the limits, without a model. The newest
/// release keeps [`PER_RELEASE`] lines; then the older ones, oldest first, go
/// to [`SHRUNK`] bullets each until the lists fit, then to their link line each
/// (every one of them named, none dropped). Security lines are never dropped.
pub(super) fn shrunk(releases: &[Release]) -> Vec<Vec<String>> {
    let mut lists: Vec<Vec<String>> = releases.iter().map(full).collect();
    let n = lists.len();
    if let Some(newest) = lists.first_mut() {
        *newest = keep(newest, PER_RELEASE);
    }
    for i in (1..n).rev() {
        if fits(&lists) {
            break;
        }
        lists[i] = keep(&lists[i], SHRUNK);
    }
    for i in (1..n).rev() {
        if fits(&lists) {
            break;
        }
        if releases[i].has_notes() {
            lists[i] = only_link(&releases[i]);
        }
    }
    collapse(releases, &mut lists);
    lists
}

/// Makes `lists` fit the total limit when even the links do not: the newest
/// releases stay as they are, as many as fit, and the rest become one
/// `see <link> for the older releases` line under the last one kept, with the
/// security lines of the rest. Nothing changes when no such split fits.
fn collapse(releases: &[Release], lists: &mut Vec<Vec<String>>) {
    if fits(lists) {
        return;
    }
    let n = lists.len();
    for kept in (1..n).rev() {
        let Some(rest) = releases[kept..].iter().find(|r| r.has_notes()) else {
            continue;
        };
        let mut out: Vec<Vec<String>> = lists[..kept].to_vec();
        let last = &mut out[kept - 1];
        last.extend(
            lists[kept..]
                .iter()
                .flatten()
                .filter(|i| is_security(i))
                .cloned(),
        );
        last.push(format!("see {} for the older releases", rest.link));
        out.extend(std::iter::repeat_n(Vec::new(), n - kept));
        if fits(&out) {
            *lists = out;
            return;
        }
    }
}

/// The lists of `releases` when the notes do not fit the limits: the model's
/// per-release recap when `condense` gives one that holds (see
/// [`agent_recap::checked`]), else the mechanical shrink. `condense` is asked
/// only here, never for notes that fit; it returns `None` without a model.
fn condensed(releases: &[Release], condense: impl Fn(&str) -> Option<String>) -> Vec<Vec<String>> {
    let Some(answer) = condense(&agent_recap::notes_of(releases)) else {
        return shrunk(releases);
    };
    match agent_recap::checked(releases, &answer) {
        Ok(sections) => {
            let mut sections = sections.into_iter();
            releases
                .iter()
                .map(|r| {
                    if r.has_notes() {
                        sections.next().unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                })
                .collect()
        }
        Err(why) => {
            ap_core::diag!("  ✗ release notes recap not used ({why}): shrinking it instead");
            shrunk(releases)
        }
    }
}

/// The releases of the window per version: a version's changelog entry when
/// the changelog has notes for it, else the forge's entry for it.
fn per_version(changelog: Vec<Found>, forge: Vec<Found>) -> Vec<Found> {
    let mut found: Vec<Found> = changelog
        .into_iter()
        .filter(|f| release_of(f).has_notes())
        .collect();
    let covered: Vec<String> = found.iter().map(|f| f.version.clone()).collect();
    found.extend(forge.into_iter().filter(|f| {
        !covered
            .iter()
            .any(|v| version::sort_cmp(v, &f.version) == Ordering::Equal)
    }));
    found
}

/// The recap over several releases of the jump `old` to `new`, or `None` when
/// the window holds one release (the single-release form applies). `changelog`
/// and `forge` are the releases each source finds in the window; each version
/// takes its changelog entry when that has notes, else its forge entry. The
/// first entry is always `new`, even without notes (the update's own bullet);
/// an older release gets an entry only when it has lines. Empty when no
/// release has lines at all.
pub(super) fn several(
    new: &str,
    old: &str,
    changelog: Vec<Found>,
    forge: Vec<Found>,
    condense: impl Fn(&str) -> Option<String>,
) -> Option<Vec<ReleaseNotes>> {
    let mut versions: Vec<String> = changelog
        .iter()
        .chain(&forge)
        .map(|f| f.version.clone())
        .filter(|v| version::sort_cmp(v, old) == Ordering::Greater)
        .collect();
    versions.sort_by(|a, b| version::sort_cmp(b, a));
    versions.dedup_by(|a, b| version::sort_cmp(a, b) == Ordering::Equal);
    if versions.len() < 2 {
        return None;
    }
    let mut found = per_version(changelog, forge);
    found.sort_by(|a, b| version::sort_cmp(&b.version, &a.version));
    let mut releases: Vec<Release> = found
        .iter()
        .map(release_of)
        .filter(Release::has_notes)
        .collect();
    let own_first = releases
        .first()
        .is_some_and(|r| version::sort_cmp(&r.version, new) == Ordering::Equal);
    if !own_first {
        releases.insert(
            0,
            Release {
                version: new.to_string(),
                link: String::new(),
                bullets: Vec::new(),
                prose: None,
            },
        );
    }
    let lists = match fitting(&releases) {
        Some(lists) => lists,
        None => condensed(&releases, condense),
    };
    let out: Vec<ReleaseNotes> = releases
        .iter()
        .zip(lists)
        .enumerate()
        .filter(|(i, (_, list))| *i == 0 || !list.is_empty())
        .map(|(i, (r, list))| {
            let version = if i == 0 { new } else { r.version.as_str() };
            (version.to_string(), render::bullet_lines(&list))
        })
        .collect();
    if out.iter().all(|(_, lines)| lines.is_empty()) {
        return Some(Vec::new());
    }
    Some(out)
}
