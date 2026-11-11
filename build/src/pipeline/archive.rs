//! The release archives of a package: finding them, their digests, fetching them,
//! and the run state and spend they update.

use super::*;
use crate::pipeline::prompt_notes::*;

/// The suffixes of a tarball the extractor reads (`classify` in ecosystem): the
/// tar family, the short spellings too.
const TARBALL_SUFFIXES: [&str; 8] = [
    ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".tar.zst", ".tzst",
];

/// The suffix of an uncompressed tar. No extractor reads it, so it is never a
/// source; a replaced release's bare tar is still deleted (see `is_tarball_name`).
const BARE_TAR_SUFFIX: &str = ".tar";

/// The suffixes of the other source archives: zip and wheels.
const ZIP_WHEEL_SUFFIXES: [&str; 2] = [".zip", ".whl"];

/// Whether `name` is a Cargo dep bundle (`vendor.tar*`, `registry.tar*`), which
/// the vendoring services make: never a source archive.
fn is_dep_bundle(name: &str) -> bool {
    matches!(
        name.split_once('.'),
        Some(("vendor" | "registry", rest)) if rest.starts_with("tar")
    )
}

/// Whether `name` is a tarball that is a source: one the extractor reads.
fn is_extractable_tarball(name: &str) -> bool {
    !is_dep_bundle(name) && TARBALL_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Whether `name` is the file name of a tarball of a release, for the deletion
/// of the release a new one replaces (carry): an extractable tarball or a bare
/// `.tar`, which is not a source and so is never unpacked.
pub(crate) fn is_tarball_name(name: &str) -> bool {
    is_extractable_tarball(name) || (!is_dep_bundle(name) && name.ends_with(BARE_TAR_SUFFIX))
}

/// Whether `name` is the file name of a source archive: a tarball the extractor
/// reads, a zip or a wheel. The one list of source archive names: `find_tarball`,
/// the doctor's checks and `source_archives` use it.
pub(crate) fn is_source_archive_name(name: &str) -> bool {
    is_extractable_tarball(name)
        || (!is_dep_bundle(name) && ZIP_WHEEL_SUFFIXES.iter().any(|s| name.ends_with(s)))
}

/// The source archives in `dir`, in directory order.
pub(super) fn source_archives(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let e = e.ok()?;
            is_source_archive_name(&e.file_name().to_string_lossy()).then(|| e.path())
        })
        .collect()
}

/// The source archive in the checkout. A checkout can hold several (an old
/// version's tarball next to the new one), so prefer the one named for
/// `version` and break ties by name, never by directory order.
pub(crate) fn find_tarball(dir: &Path, version: &str) -> Option<PathBuf> {
    let mut found = source_archives(dir);
    // Ranked by (the main archive `<name>-<version>.<ext>`, a name that carries
    // the version); `true` sorts first, then the path decides.
    let rank = |p: &PathBuf| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (
            !version.is_empty() && is_main_archive(&name, version),
            !version.is_empty() && names_version(&name, version),
        )
    };
    found.sort_by(|a, b| rank(b).cmp(&rank(a)).then_with(|| a.cmp(b)));
    found.into_iter().next()
}

/// Whether `name` is the main source archive of `version`: the version ends the
/// project name and the archive extension follows it directly
/// (`foo-1.2.tar.gz`, `foo_1.2.zip`), with nothing in between (no `-cpp`, `-docs`).
pub(super) fn is_main_archive(name: &str, version: &str) -> bool {
    const EXTS: [&str; 6] = [".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip"];
    EXTS.iter().any(|ext| {
        name.strip_suffix(ext)
            .and_then(|stem| stem.strip_suffix(version))
            .is_some_and(|before| before.len() >= 2 && before.ends_with(['-', '_']))
    })
}

/// Whether the archive file name `name` carries `version` as a whole version:
/// `1.2` is in `pkg-1.2.tar.gz` and `pkg-1.2-py3.whl`, but not in `pkg-1.2.1.tar.gz`,
/// `pkg-13.3.1.tar.gz` (for `3.1`) or `pkg-11.2.tar.gz`. The version must follow
/// the `-` or `_` that ends the project name (or start the name). What follows
/// the version is the end of the name, an archive extension, or a wheel tag
/// (`-py3-none-any.whl`, `-cp311-...`): a suffix such as `-docs` or `-data`
/// names another archive, not this version's main one. A wheel takes any tag
/// after the version (`-pp310-...`, a build tag `-1-py3-none-any`): every wheel
/// name is a tag list, so none of them is another archive.
pub(super) fn names_version(name: &str, version: &str) -> bool {
    let is_wheel = name.ends_with(".whl");
    !version.is_empty()
        && name.match_indices(version).any(|(i, _)| {
            let starts_here = name[..i]
                .chars()
                .last()
                .is_none_or(|c| c == '-' || c == '_');
            let rest = &name[i + version.len()..];
            starts_here
                && (rest.is_empty()
                    || [".tar", ".zip", ".whl", ".tgz"]
                        .iter()
                        .any(|ext| rest.starts_with(ext))
                    // Only a wheel's name is a tag list: `-cpp.tar.gz` or
                    // `-python.tar.gz` is another archive, not this version's.
                    || (is_wheel && rest.starts_with('-')))
        })
}

/// The tarballs of `before` (the checkout's tarballs before the service ran)
/// that the new release replaces: those that carry `old_version` and not
/// `new_version`, other than `made` (the new tarball's file name) and the
/// sources of the checkout's sibling specs. Sorted.
pub(super) fn superseded_tarballs(
    before: &std::collections::HashSet<String>,
    old_version: &str,
    new_version: &str,
    made: &str,
    siblings: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = before
        .iter()
        .filter(|old| {
            old.contains(old_version)
                && !old.contains(new_version)
                && old.as_str() != made
                && !siblings.contains(old)
        })
        .cloned()
        .collect();
    out.sort();
    out
}

/// The source archive of `old_version` in `dir` that a new version replaces.
/// `keep` (the new tarball) is never returned, and a longer version that
/// merely starts with `old_version` (`1.2.1` for `1.2`) does not match. An
/// archive the new spec still names as a source (`Source1:` for a second
/// tarball of the old version) is not superseded either.
pub(super) fn superseded_tarball(
    dir: &Path,
    old_version: &str,
    keep: &Path,
    new_sources: &[crate::spec::SourceEntry],
) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            let archive = [
                ".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip", ".whl",
            ]
            .iter()
            .any(|ext| name.ends_with(ext));
            (archive
                && path != keep
                && !matches!(name.as_str(), "vendor.tar.zst" | "registry.tar.zst")
                && names_version(&name, old_version)
                && is_superseded_source_file(&name, new_sources))
            .then_some(path)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

pub(super) fn find_sdist_top_level(checkout_dir: &Path, pkg_name: &str, version: &str) -> String {
    if let Some(tb) = find_tarball(checkout_dir, version) {
        let fname = tb.file_name().unwrap_or_default().to_string_lossy();
        // A wheel unpacks flat (no wrapping directory) — see
        // `ap_ecosystem::sdist::extract`'s `.whl` branch.
        if fname.ends_with(".whl") {
            return String::new();
        }
        // The directory inside the archive is not always named after the file:
        // `AMQPStorm-3.1.4.tar.gz` holds `amqpstorm-3.1.4/`. The archive's own
        // top-level directory is the one the sources unpack into.
        if let Ok(info) = ap_ecosystem::sdist::inspect(&tb)
            && let Some(top) = info.top_level_dir
        {
            return top;
        }
        for ext in &[".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip"] {
            if let Some(stem) = fname.strip_suffix(ext) {
                return stem.to_string();
            }
        }
    }
    format!("{}-{}", pkg_name.to_lowercase(), version)
}

/// SHA-256 of a file, lowercase hex, read in chunks.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Record which archive a spec was generated from, so the commit to OBS keeps
/// a digest of what was used.
pub(super) fn record_archive_digest(spec_path: &Path, url: &str, digest: &str) {
    let noted = std::fs::read_to_string(spec_path).and_then(|spec| {
        ap_core::fsx::write_atomic(
            spec_path,
            crate::spec_tidy::with_archive_note(&spec, url, digest),
        )
    });
    if let Err(e) = noted {
        ap_core::diag!(
            "  could not record the archive digest in {}: {e}",
            spec_path.display()
        );
    }
}

/// Put the first release archive that exists at `dest`, returning its URL.
///
/// A 404 only means that candidate is not this release. Any other failure
/// (timeout, dropped connection, rate limit, size cap) is no evidence that the
/// archive does not exist, so it is reported as a failure, not as a missing
/// archive: the caller falls back to PyPI only for the latter.
/// The suffix a release archive is saved under: the kind its URL names, so the
/// inspector picks the decoder from the file name (`.tgz` is saved as
/// `.tar.gz`). `None` for a kind the inspector cannot read. Mirrors the kinds
/// `ap_ecosystem::sdist` opens.
pub(super) fn archive_save_suffix(url: &str) -> Option<&'static str> {
    let lower = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    const KINDS: [(&str, &str); 9] = [
        (".tar.gz", ".tar.gz"),
        (".tgz", ".tar.gz"),
        (".tar.bz2", ".tar.bz2"),
        (".tbz2", ".tar.bz2"),
        (".tar.zst", ".tar.zst"),
        (".tzst", ".tar.zst"),
        (".tar.xz", ".tar.xz"),
        (".txz", ".tar.xz"),
        (".zip", ".zip"),
    ];
    KINDS
        .iter()
        .find(|(ext, _)| lower.ends_with(ext))
        .map(|(_, save)| *save)
}

/// Download the release archive of `version` from `repo_url`, saved as
/// `dest_stem` plus the suffix of its own kind. Returns the URL it came from
/// and the file it was saved to. A candidate of a kind the inspector cannot
/// read is refused before anything is downloaded.
pub(super) fn fetch_release_archive(
    repo_url: &str,
    version: &str,
    dest_stem: &Path,
) -> Result<(String, PathBuf), GenSpecError> {
    let candidates = crate::sources::archive_urls(repo_url, version);
    let mut failures = Vec::new();
    for url in &candidates {
        let Some(suffix) = archive_save_suffix(url) else {
            return Err(GenSpecError::Other(format!(
                "{url} is not a release archive this tool can read (expected .tar.gz/.tgz, \
                 .tar.bz2/.tbz2, .tar.xz/.txz, .tar.zst/.tzst or .zip); not downloaded"
            )));
        };
        let mut dest = dest_stem.as_os_str().to_owned();
        dest.push(suffix);
        let dest = PathBuf::from(dest);
        match crate::sources::fetch_archive_to(url, &dest) {
            Ok(()) => return Ok((url.clone(), dest)),
            Err(crate::http::FetchFailure::NotFound) => {}
            Err(e) => failures.push(format!("{url}: {e}")),
        }
    }
    if !failures.is_empty() {
        return Err(GenSpecError::Other(format!(
            "could not download a release archive for {version} from {repo_url}:\n  {}",
            failures.join("\n  ")
        )));
    }
    Err(GenSpecError::NoReleaseArchive(format!(
        "no release archive for version {version} at {repo_url}. Tried:\n{}",
        candidates
            .iter()
            .map(|u| format!("  {u}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )))
}

/// Build a minimal `Release` for a Python project that isn't on PyPI at
/// all (an upstream stub that was never published, a vendor-internal
/// package, a monorepo subproject). `name`/`version`/`tarball_filename`
/// are all we know; everything else (summary, license, homepage,
/// `requires_python`) is left blank for the Review agent to fill in
/// from the pyproject.toml during the first fix turn — the same path
/// the monorepo skeleton already takes.
///
/// `sdist.sha256`/`url`/`size` stay empty for the same reason
/// `parse_release_with_tarball` leaves them so: the tarball has
/// already been downloaded locally, and the generator uses only the
/// filename.
pub(super) fn synthesize_release_without_pypi(
    name: String,
    version: String,
    tarball_filename: String,
) -> ap_ecosystem::pypi::Release {
    ap_ecosystem::pypi::Release {
        name,
        version,
        summary: None,
        homepage: None,
        license: ap_ecosystem::pypi::License::Unknown,
        requires_python: None,
        requires_dist: Vec::new(),
        yanked: false,
        sdist: ap_ecosystem::pypi::Sdist {
            filename: tarball_filename,
            url: String::new(),
            sha256: String::new(),
            size: 0,
        },
    }
}

/// `https://pypi.org/pypi/<name>/<version>/json` with both parts URL-encoded.
pub(super) fn pypi_json_url(name: &str, version: &str) -> String {
    format!(
        "https://pypi.org/pypi/{}/{}/json",
        urlencoding::encode(name),
        urlencoding::encode(version)
    )
}

/// PyPI's JSON for a release: `Ok(None)` when the project/version is simply not
/// on PyPI (404), `Err` for anything else (network, 5xx, oversized body) so the
/// caller can tell "not published" from "could not ask".
pub fn fetch_pypi_json(name: &str, version: &str) -> Result<Option<String>, String> {
    match crate::http::get_with_retries(&pypi_json_url(name, version)) {
        Ok(resp) => crate::http::read_text(resp)
            .map(Some)
            .map_err(|e| format!("reading PyPI JSON for {name} {version}: {e}")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(format!("PyPI JSON for {name} {version}: {e}")),
    }
}

/// A component of a file name built from untrusted text (package name, tag):
/// anything but `[A-Za-z0-9._+-]` becomes `_`, and `..` cannot survive.
pub(super) fn update_state(ws: &Workspace, reporter: &dyn Reporter, f: impl FnOnce(&mut PkgState)) {
    warn_if_err(reporter, "saving state.json", PkgState::update(ws, f));
}

/// Tag the per-run counters with `run_id`. Stamping this run's id onto spend or
/// attempts recorded under another run would make that run's dollars and
/// attempts count against this one, so both reset when the id changes.
pub(super) fn claim_run(st: &mut PkgState, run_id: &str) {
    if st.cumulative_spent_run_id.as_deref() != Some(run_id) {
        st.cumulative_spent_usd = 0.0;
        st.cumulative_attempts = 0;
        st.cumulative_build_rounds = 0;
    }
    st.cumulative_spent_run_id = Some(run_id.to_string());
}

/// Add a turn's cost to the package's spend for this run, so state.json keeps
/// it even when the pipeline stops before the next successful turn. Spend from
/// an earlier run is not carried over (see `PkgState::cumulative_spent_run_id`).
pub(super) fn add_run_spend(ws: &Workspace, run_id: &str, cost_usd: f64) {
    // A read-modify-write under the state lock: `base` must come from the
    // file as it is now, not from a copy another writer has since replaced.
    let saved = PkgState::update(ws, |st| {
        claim_run(st, run_id);
        st.cumulative_spent_usd += cost_usd;
    });
    if let Err(e) = saved {
        ap_core::diag!("  cannot save the notes cost: {e}");
    }
}
