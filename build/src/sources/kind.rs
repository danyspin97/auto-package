// ── non-forge sources: release directories and direct tarballs ─────────────────
//
// Plenty of upstreams (GNU Savannah, ftp.gnu.org, self-hosted project sites)
// have no forge API at all: just a directory of release tarballs. They are
// recognised by shape, not by a hard-coded host list:
//
// * a URL whose last segment is a tarball is a *direct archive*;
// * a URL on an unknown host with an index-like path segment
//   (`/releases/`, `/pub/`, `/gnu/`, ...) is a *release index* — an HTML
//   directory listing whose links are the tarballs.
//
// Only `.tar.gz`/`.tgz` are used: every later stage stores and extracts the
// download as gzip, and an `.xz`-only release is reported as unsupported
// rather than silently mis-extracted.

/// How a source URL is to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A git forge with a REST API (GitHub, GitLab, Gitea/Forgejo).
    Forge,
    /// A directory listing of release tarballs.
    ReleaseIndex,
    /// One specific release tarball.
    DirectArchive,
}

pub(super) const GZIP_EXTS: &[&str] = &[".tar.gz", ".tgz"];

pub(super) const OTHER_ARCHIVE_EXTS: &[&str] = &[
    ".tar.xz", ".txz", ".tar.lz", ".tar.bz2", ".tbz2", ".tar.zst", ".zip",
];

pub(super) const INDEX_HINTS: &[&str] = &[
    "releases",
    "release",
    "pub",
    "gnu",
    "files",
    "download",
    "downloads",
    "dist",
    "distfiles",
    "sources",
    "src",
    "ftp",
    "mirror",
    "archive",
];

pub(crate) fn last_segment(url: &str) -> &str {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
}

pub(super) fn is_gzip_archive(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    GZIP_EXTS.iter().any(|e| f.ends_with(e))
}

pub(crate) fn is_any_archive(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    GZIP_EXTS
        .iter()
        .chain(OTHER_ARCHIVE_EXTS)
        .any(|e| f.ends_with(e))
}

/// Classify a source URL. See the section comment above.
pub fn source_kind(url: &str) -> SourceKind {
    // A file under `/releases/download/` or `/archive/` is a fixed asset: the
    // forge-first rule below would turn it into a lookup of the latest tag and
    // package a different version than the URL names.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if (path.contains("/releases/download/") || path.contains("/archive/"))
        && is_any_archive(last_segment(url))
    {
        return SourceKind::DirectArchive;
    }
    // Other forge URLs (repositories, tags) before archive extensions, so a
    // tag URL is still resolved through the forge.
    if let Some(forge) = ap_ecosystem::forge::parse(url)
        && ap_ecosystem::forge::is_known_host(&forge.host)
    {
        return SourceKind::Forge;
    }
    if is_any_archive(last_segment(url)) {
        return SourceKind::DirectArchive;
    }
    let Some(forge) = ap_ecosystem::forge::parse(url) else {
        return SourceKind::Forge;
    };
    let indexish = forge
        .project_path
        .split('/')
        .any(|seg| INDEX_HINTS.contains(&seg.to_ascii_lowercase().as_str()));
    if indexish {
        SourceKind::ReleaseIndex
    } else {
        SourceKind::Forge
    }
}

/// `(name, version)` from a release tarball filename: `foo-1.2.3.tar.gz` →
/// `("foo", "1.2.3")`. The split is at the last `-` or `_` that is followed
/// by a digit, so `foo-bar-2.0.tar.gz` → `("foo-bar", "2.0")`.
///
/// A trailing pure-digit part after a dotted version is a release counter of
/// the distribution, not a version of its own: `dmraid-1.0.0.rc16-3.tar.bz2`
/// is `("dmraid", "1.0.0.rc16-3")` and `mawk-1.3.4-20260302.tgz` is
/// `("mawk", "1.3.4-20260302")`.
pub fn archive_name_version(file: &str) -> Option<(String, String)> {
    let lower = file.to_ascii_lowercase();
    let ext = GZIP_EXTS
        .iter()
        .chain(OTHER_ARCHIVE_EXTS)
        .find(|e| lower.ends_with(**e))?;
    let stem = &file[..file.len() - ext.len()];
    let idx = last_version_separator(stem)?;
    let (name, version) = (&stem[..idx], &stem[idx + 1..]);
    if version.bytes().all(|b| b.is_ascii_digit())
        && let Some(cut) = dotted_version_separator(&stem[..idx])
    {
        return Some((stem[..cut].to_string(), stem[cut + 1..].to_string()));
    }
    (!name.is_empty()).then(|| (name.to_string(), version.to_string()))
}

/// The byte index of the last `-` or `_` that is followed by a digit.
fn last_version_separator(stem: &str) -> Option<usize> {
    stem.char_indices()
        .rev()
        .find(|&(i, c)| {
            (c == '-' || c == '_') && stem[i + 1..].starts_with(|d: char| d.is_ascii_digit())
        })
        .map(|(i, _)| i)
}

/// For `pre`, the part of a file name before its release counter: the last
/// `-` or `_` before a digit run that holds a `.` (`-1.0.0.rc16` in
/// `dmraid-1.0.0.rc16`). `None` when there is no such part or no name before it.
fn dotted_version_separator(pre: &str) -> Option<usize> {
    let j = last_version_separator(pre)?;
    (j > 0 && pre[j + 1..].contains('.')).then_some(j)
}

/// Link targets of an HTML directory listing, reduced to their last path
/// segment. Queries, anchors, parent/sort links and absolute foreign links
/// are dropped; entity-escaped ampersands are not an issue because only the
/// file name survives.
pub fn parse_index_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Lowercased once: ASCII lowercasing keeps byte offsets, and doing it per
    // link made a large listing quadratic.
    let lower = html.to_ascii_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find("href=") {
        let after = &html[pos + i + 5..];
        let (quote, body) = match after.chars().next() {
            Some(q @ ('"' | '\'')) => (q, &after[1..]),
            _ => {
                pos += i + 5;
                continue;
            }
        };
        let Some(end) = body.find(quote) else { break };
        let target = &body[..end];
        pos = html.len() - body.len() + end;
        let file = last_segment(target);
        if !file.is_empty()
            && !target.contains("..")
            && !target.starts_with('?')
            && !target.starts_with('#')
            && seen.insert(file.to_string())
        {
            out.push(file.to_string());
        }
    }
    out
}

/// See [`crate::version::sort_cmp`].
pub(crate) fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    crate::version::sort_cmp(a, b)
}

/// See [`crate::version::is_prerelease`].
pub(crate) fn is_prerelease(version: &str) -> bool {
    crate::version::is_prerelease(version)
}

/// The release a version split off an archive name stands for: a trailing
/// source marker is not part of it (`4.3.8.src` is 4.3.8, `3.9.5-src` 3.9.5),
/// and `None` for an archive that is a build for one platform or flavor
/// rather than the release (`1.10-win32-dll-openssl`).
pub(crate) fn release_version(version: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        ".src", "-src", "_src", ".source", "-source", "_source", ".orig",
    ];
    let lower = version.to_ascii_lowercase();
    let mut end = version.len();
    for m in MARKERS {
        if lower.ends_with(m) {
            end = version.len() - m.len();
            break;
        }
    }
    let v = &version[..end];
    // A word that is neither a pre-release tag nor a release suffix names a
    // platform or a flavor (win32, dll, openssl, linux, bin).
    let flavored = v.split(['.', '-', '_']).any(|t| {
        let letters = t.chars().filter(char::is_ascii_alphabetic).count();
        letters >= 3
            && !is_prerelease(t)
            && !["stable", "final", "release", "patch", "post", "p"]
                .contains(&t.to_ascii_lowercase().as_str())
    });
    (!v.is_empty() && v.starts_with(|c: char| c.is_ascii_digit()) && !flavored)
        .then(|| v.to_string())
}
