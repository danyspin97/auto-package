//! The source files a review downloads: the signed archives with their
//! signatures and the keyring, and the main source archive, laid out flat in
//! one directory as a checkout is, so the signature check reads them. The
//! listed sizes decide before any download.

use std::path::{Path, PathBuf};

use super::fetch::RequestObs;
use super::gather::{ActionInputs, Skipped};
use super::{Finding, Severity, ids};
use crate::pipeline::{SIGNATURE_SUFFIXES, archive_of};

/// The most a review downloads of one action, by the sizes the listing gives.
pub const REVIEW_DOWNLOAD_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// What was downloaded for one action.
#[derive(Debug, Default)]
pub struct Downloads {
    pub dir: PathBuf,
    /// The names that are in `dir`.
    pub fetched: Vec<String>,
    /// The main source archive, when it was downloaded.
    pub main_archive: Option<PathBuf>,
    pub skipped: Vec<Skipped>,
    pub findings: Vec<Finding>,
    /// A file the signature check needs could not be downloaded: the review
    /// cannot be completed (the signatures are neither checked nor skipped on
    /// purpose).
    pub required_failure: Option<String>,
}

fn is_signature(name: &str) -> bool {
    SIGNATURE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// The files to download, in order, each with whether the signature check needs
/// it: each signature that the request adds or changes with its archive, the
/// keyring, then the main archive.
fn wanted(a: &ActionInputs) -> Vec<(String, bool)> {
    let touched = |n: &str| a.files.added.iter().chain(&a.files.changed).any(|t| t == n);
    let names = a.new.file_names();
    let mut out: Vec<String> = Vec::new();
    for sig in names.iter().filter(|n| is_signature(n)) {
        let archive = archive_of(sig);
        if touched(sig) || touched(&archive) {
            out.push(archive);
            out.push(sig.clone());
        }
    }
    if !out.is_empty() {
        out.extend(names.iter().filter(|n| n.ends_with(".keyring")).cloned());
    }
    let (sources, _) = a
        .new
        .spec()
        .map(crate::spec::declared_source_files)
        .unwrap_or_default();
    if let Some(main) = sources.into_iter().next() {
        out.push(main);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| names.contains(n) && seen.insert(n.clone()));
    out.into_iter()
        .map(|n| {
            let signed = is_signature(&n)
                || n.ends_with(".keyring")
                || names.iter().any(|s| is_signature(s) && archive_of(s) == n);
            (n, signed)
        })
        .collect()
}

/// Why nothing is downloaded: the checks that need the bytes did not run.
pub fn download_disabled(a: &ActionInputs) -> Vec<Skipped> {
    let signed = a.new.file_names().iter().any(|n| is_signature(n));
    let mut out = vec![Skipped {
        check: "source archive".into(),
        why: "downloads are off (--no-download): patch paths and vendor archives were not checked"
            .into(),
    }];
    if signed {
        out.push(Skipped {
            check: "signatures".into(),
            why: "downloads are off (--no-download)".into(),
        });
    }
    out
}

/// Download what the checks need of `a` into `dir`. A failure is a
/// `review.incomplete` finding.
pub fn download_sources(obs: &dyn RequestObs, a: &ActionInputs, dir: &Path) -> Downloads {
    let mut out = Downloads {
        dir: dir.to_path_buf(),
        ..Downloads::default()
    };
    let Some(rev) = a.source.rev.as_deref().or(a.source_srcmd5.as_deref()) else {
        out.skipped.push(Skipped {
            check: "downloads".into(),
            why: "the action names no revision and none could be read".into(),
        });
        return out;
    };
    if let Err(e) = std::fs::create_dir_all(dir.join(".fetch")) {
        out.skipped.push(Skipped {
            check: "downloads".into(),
            why: format!("cannot create {}: {e}", dir.display()),
        });
        return out;
    }
    let mut total = 0u64;
    for (name, needed) in wanted(a) {
        let size = a
            .new
            .listing
            .files
            .iter()
            .find(|f| f.name == name)
            .map_or(0, |f| f.size);
        if total + size > REVIEW_DOWNLOAD_MAX_BYTES {
            out.skipped.push(Skipped {
                check: name.clone(),
                why: format!("not downloaded: {size} bytes would pass the {REVIEW_DOWNLOAD_MAX_BYTES} byte cap"),
            });
            continue;
        }
        let fetched = obs
            .download(
                &a.source.project,
                &a.source.package,
                &name,
                rev,
                &dir.join(".fetch"),
            )
            .and_then(|got| {
                std::fs::rename(&got, dir.join(&name)).map_err(|e| {
                    super::fetch::ReadFailure::new(format!("cannot place {name}: {e}"))
                })
            });
        match fetched {
            Ok(()) => {
                total += size;
                out.fetched.push(name.clone());
            }
            Err(e) if needed => {
                out.required_failure
                    .get_or_insert(format!("{name} could not be downloaded: {e}"));
            }
            Err(e) => out.findings.push(Finding::check(
                ids::REVIEW_INCOMPLETE,
                Severity::Note,
                &a.target.package,
                &format!("{name} could not be downloaded and was not checked: {e}"),
            )),
        }
    }
    let _ = std::fs::remove_dir_all(dir.join(".fetch"));
    let main = crate::spec::declared_source_files(a.new.spec().unwrap_or(""))
        .0
        .into_iter()
        .next();
    out.main_archive = main
        .filter(|m| out.fetched.contains(m))
        .map(|m| dir.join(m));
    out
}
