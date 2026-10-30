//! The recap of an update for the `.changes` file: what the releases between
//! the packaged version and the new one say changed.
//!
//! Short bullet notes are listed as they are, long or prose notes are
//! condensed by one model call, and when they still are too many the entry
//! links the release pages. A package with no notes says so.

mod render;
mod summarize;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use crate::sources::RepoRelease;
use crate::version;

use render::Notes;

/// The recap lines (`* item`, continuation lines indented) for updating a
/// package from `old` to `new`, whose source is the repository `repo_url`.
/// Never empty: a package whose notes cannot be found says so.
pub fn update_notes(repo_url: Option<&str>, old: Option<&str>, new: &str) -> Vec<String> {
    let releases = match (repo_url, old) {
        (Some(url), Some(old)) => match crate::sources::list_releases(url) {
            Ok(all) => releases_between(&all, url, old, new),
            Err(e) => {
                ap_core::diag!("  ✗ could not list the releases of {url}: {e}");
                Vec::new()
            }
        },
        _ => Vec::new(),
    };
    recap(&releases, summarize::summarize)
}

/// The recap of `releases` (newest first), the summary of long notes coming
/// from `summarize`.
pub(super) fn recap(
    releases: &[RepoRelease],
    summarize: impl Fn(&str) -> Option<Vec<String>>,
) -> Vec<String> {
    match render::classify(releases) {
        Notes::None => render::no_notes_line(),
        Notes::Bullets(b) => render::bullet_lines(&b),
        Notes::Long(text) => match summarize(&text) {
            Some(b) => render::bullet_lines(&b),
            None => render::link_lines(releases),
        },
    }
}

/// The releases newer than `old` and not newer than `new`, newest first. The
/// version of a release is its tag without a project prefix or leading `v`.
pub(super) fn releases_between(
    all: &[RepoRelease],
    repo_url: &str,
    old: &str,
    new: &str,
) -> Vec<RepoRelease> {
    let repo = ap_ecosystem::forge::parse(repo_url)
        .map(|f| f.repo_name)
        .unwrap_or_default();
    let mut between: Vec<(String, RepoRelease)> = all
        .iter()
        .map(|r| (crate::sources::version_from_tag(&r.tag, &repo), r.clone()))
        .filter(|(v, _)| {
            version::sort_cmp(v, old) == Ordering::Greater
                && version::sort_cmp(v, new) != Ordering::Greater
        })
        .collect();
    between.sort_by(|a, b| version::sort_cmp(&b.0, &a.0));
    between.into_iter().map(|(_, r)| r).collect()
}
