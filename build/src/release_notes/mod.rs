//! The recap of an update for the `.changes` file: what the releases between
//! the packaged version and the new one say changed.
//!
//! Short bullet notes are listed as they are, long or prose notes are
//! condensed by one model call, and when they still are too many the entry
//! links the release pages. A package with no notes says so.

mod changelog;
mod render;
mod summarize;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use crate::sources::RepoRelease;
use crate::version;

use render::Notes;

/// Writes the recap of updating from `old` to `new` under the `Update to`
/// bullet of the newest `.changes` entry in `checkout_dir`, reading the new
/// source archive from there when the checkout holds it.
pub fn add_recap(
    checkout_dir: &std::path::Path,
    repo_url: Option<&str>,
    old: Option<&str>,
    new: &str,
) -> Result<(), crate::osc::Error> {
    let archive = archive_in(checkout_dir, new);
    let lines = update_notes(repo_url, archive.as_deref(), old, new);
    crate::osc::set_update_notes(checkout_dir, new, &lines)
}

/// The source archive of `version` in `dir`: a tarball whose name carries the
/// version, never a dependency bundle.
fn archive_in(dir: &std::path::Path, version: &str) -> Option<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned());
            n.is_some_and(|n| {
                n.contains(version)
                    && !n.starts_with("vendor.")
                    && !n.starts_with("registry.")
                    && [".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip"]
                        .iter()
                        .any(|e| n.ends_with(e))
            })
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The recap lines (`* item`, continuation lines indented) for updating a
/// package from `old` to `new`. The changelog file in the new source archive
/// is read first; the notes of the releases on the forge of `repo_url` are the
/// fallback. Never empty: a package whose notes cannot be found says so.
pub fn update_notes(
    repo_url: Option<&str>,
    archive: Option<&std::path::Path>,
    old: Option<&str>,
    new: &str,
) -> Vec<String> {
    let section = old.and_then(|old| {
        let fetched;
        let archive = match archive {
            Some(a) => a,
            None => {
                fetched = repo_url.and_then(|u| {
                    crate::sources::archive_urls(u, new)
                        .iter()
                        .find_map(|url| crate::sources::fetch_archive(url).ok())
                })?;
                &fetched
            }
        };
        changelog::section_of_archive(archive, old, new)
    });
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
    recap(section, &releases, summarize::summarize)
}

/// The recap from the changelog `section` of the sources, else from the notes
/// of `releases` (newest first). Long notes are condensed by `summarize`.
fn recap(
    section: Option<changelog::Section>,
    releases: &[RepoRelease],
    summarize: impl Fn(&str) -> Option<Vec<String>>,
) -> Vec<String> {
    // A changelog that is too long for bullets and cannot be summarized still
    // says where to read it, when the releases have nothing better.
    let mut too_long = None;
    if let Some(sec) = section {
        let as_release = RepoRelease {
            tag: sec.file.clone(),
            url: String::new(),
            body: sec.text,
        };
        match render::classify(std::slice::from_ref(&as_release)) {
            Notes::Bullets(b) => return render::bullet_lines(&b),
            Notes::Long(text) => match summarize(&text) {
                Some(b) => return render::bullet_lines(&b),
                None => too_long = Some(render::file_line(&sec.file)),
            },
            Notes::None => {}
        }
    }
    match render::classify(releases) {
        Notes::Bullets(b) => render::bullet_lines(&b),
        Notes::Long(text) => match summarize(&text) {
            Some(b) => render::bullet_lines(&b),
            None => render::link_lines(releases),
        },
        Notes::None => too_long.unwrap_or_else(render::no_notes_line),
    }
}

/// The releases newer than `old` and not newer than `new`, newest first. The
/// version of a release is its tag without a project prefix or leading `v`.
fn releases_between(all: &[RepoRelease], repo_url: &str, old: &str, new: &str) -> Vec<RepoRelease> {
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
