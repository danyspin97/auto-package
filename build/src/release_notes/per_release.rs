//! The recap of an update that jumps several upstream releases: the notes of
//! each release on their own, newest first, each under a bullet of its own.

use std::cmp::Ordering;

use super::changelog;
use super::markdown::strip_comments;
use super::render;
use super::routine::is_routine;
use crate::osc::ReleaseNotes;
use crate::sources::RepoRelease;
use crate::version;

/// One release of the jump: its version, where its notes are read (a release
/// page, or a changelog file in the sources) and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) version: String,
    pub(super) link: String,
    pub(super) body: String,
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

/// Whether a release has any notes at all (a comment-only body has none).
fn has_notes(f: &Found) -> bool {
    !strip_comments(&f.body).trim().is_empty()
}

/// The lines of one release: its bullets (routine ones left out), or a link
/// to the release when its notes are prose. Empty when it has nothing to list.
fn lines_of(f: &Found) -> Vec<String> {
    let listed = render::bullets_of(&f.body);
    if !listed.is_empty() {
        let kept: Vec<String> = listed.into_iter().filter(|b| !is_routine(b)).collect();
        return render::bullet_lines(&kept);
    }
    if has_notes(f) {
        vec![format!("* see {}", f.link)]
    } else {
        Vec::new()
    }
}

/// The recap of the jump `old` to `new` over several releases, or `None` when
/// the window holds one release (the single-release form applies). `changelog`
/// and `forge` are the releases each source finds in the window; the changelog
/// is read when it has notes for any of them, else the forge releases. The
/// first entry is always `new`, even without notes (the update's own bullet);
/// an older release gets an entry only when it has lines. Empty when no
/// release has lines at all.
pub(super) fn several(
    new: &str,
    old: &str,
    changelog: Vec<Found>,
    forge: Vec<Found>,
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
    let mut found = if changelog.iter().any(has_notes) {
        changelog
    } else {
        forge
    };
    found.sort_by(|a, b| version::sort_cmp(&b.version, &a.version));
    let mut out: Vec<ReleaseNotes> = vec![(new.to_string(), Vec::new())];
    for f in &found {
        let lines = lines_of(f);
        if version::sort_cmp(&f.version, new) == Ordering::Equal {
            out[0].1 = lines;
        } else if !lines.is_empty() {
            out.push((f.version.clone(), lines));
        }
    }
    if out.iter().all(|(_, lines)| lines.is_empty()) {
        return Some(Vec::new());
    }
    Some(out)
}
