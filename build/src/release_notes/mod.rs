//! The recap of an update for the `.changes` file: what the releases between
//! the packaged version and the new one say changed.
//!
//! Short bullet notes are listed as they are, long or prose notes are
//! condensed by one model call, and when they still are too many the entry
//! links the release pages. A package with no notes says so.

mod caps;
mod changelog;
#[cfg(test)]
mod clean_tests;
mod markdown;
#[cfg(test)]
mod new_package_tests;
mod per_release;
#[cfg(test)]
mod per_release_tests;
mod render;
mod routine;
mod summarize;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use crate::osc::ReleaseNotes;
use crate::sources::RepoRelease;
use crate::version;

use render::Notes;

/// What a recap found: the releases of the update, its own first and then the
/// older ones of the jump that have lines, newest first (none when nothing was
/// found), and, when a fetch failed, one short line saying so (not an error:
/// the entry just stays short).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recap {
    pub releases: Vec<ReleaseNotes>,
    pub fetch_problem: Option<String>,
}

/// Writes the recap of packaging `new` (an update from `old`, or a first
/// package when `old` is `None`) under the `Update to` bullet of the newest
/// `.changes` entry in `checkout_dir`, reading the new source archive from
/// there when the checkout holds it. With `no_agent` no model is called to
/// condense long notes. `missing` of the result is `true` when no
/// upstream notes were found: the bullet then stays alone.
pub fn add_recap(
    checkout_dir: &std::path::Path,
    repo_url: Option<&str>,
    old: Option<&str>,
    new: &str,
    no_agent: bool,
) -> Result<(bool, Option<String>), crate::osc::Error> {
    let archive = archive_in(checkout_dir, new);
    let recap = gather(repo_url, archive.as_deref(), old, new, no_agent);
    // Nothing found still names the update's own release, so an earlier recap
    // of it is removed.
    let releases = if recap.releases.is_empty() {
        vec![(new.to_string(), Vec::new())]
    } else {
        recap.releases
    };
    let missing = releases.iter().all(|(_, lines)| lines.is_empty());
    crate::osc::set_release_notes(checkout_dir, &releases)?;
    Ok((missing, recap.fetch_problem))
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

/// The recap for updating a package from `old` to `new`, or for packaging
/// `new` for the first time (`old` is `None`: then only what upstream says of
/// `new` itself counts). The changelog file in the new source archive is read
/// first; the notes of the releases on the forge of `repo_url` are the
/// fallback. Empty lines when neither has notes: the recap then says nothing.
/// With `no_agent` long notes are not condensed by a model: the links or the
/// changelog file are named instead.
pub fn gather(
    repo_url: Option<&str>,
    archive: Option<&std::path::Path>,
    old: Option<&str>,
    new: &str,
    no_agent: bool,
) -> Recap {
    let mut fetch_problem = None;
    // Older than every release heading: the section is the top one, `new`'s.
    let section = {
        let fetched;
        let archive = match archive {
            Some(a) => Some(a),
            None => {
                fetched = repo_url.and_then(|u| {
                    crate::sources::archive_urls(u, new)
                        .iter()
                        .find_map(|url| crate::sources::fetch_archive(url).ok())
                });
                fetched.as_ref().map(|f| -> &std::path::Path { f })
            }
        };
        archive.and_then(|a| changelog::section_of_archive(a, old.unwrap_or("0"), new))
    };
    let releases = match repo_url {
        Some(url) => match crate::sources::list_releases(url) {
            Ok(all) => match old {
                Some(old) => releases_between(&all, url, old, new),
                None => release_of(&all, url, new),
            },
            Err(e) => {
                ap_core::diag!("  ✗ could not list the releases of {url}: {e}");
                fetch_problem = Some(format!("could not list the releases of {url}: {e}"));
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    if let Some(old) = old {
        let changelog = section
            .as_ref()
            .map(|s| per_release::from_section(&s.file, &s.text, old, new))
            .unwrap_or_default();
        let forge = repo_url
            .map(|u| per_release::from_releases(u, &releases))
            .unwrap_or_default();
        if let Some(several) = per_release::several(new, old, changelog, forge) {
            return Recap {
                releases: several,
                fetch_problem,
            };
        }
    }
    let lines = recap(section, &releases, |text| {
        if no_agent {
            None
        } else {
            summarize::summarize(text)
        }
    });
    Recap {
        releases: if lines.is_empty() {
            Vec::new()
        } else {
            vec![(new.to_string(), lines)]
        },
        fetch_problem,
    }
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
        Notes::None => too_long.unwrap_or_default(),
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

/// The release published for exactly `new`, if any.
fn release_of(all: &[RepoRelease], repo_url: &str, new: &str) -> Vec<RepoRelease> {
    let repo = ap_ecosystem::forge::parse(repo_url)
        .map(|f| f.repo_name)
        .unwrap_or_default();
    all.iter()
        .filter(|r| {
            version::sort_cmp(&crate::sources::version_from_tag(&r.tag, &repo), new)
                == Ordering::Equal
        })
        .take(1)
        .cloned()
        .collect()
}
