//! Where a dependency's `url` comes from: an archive to download, or a git
//! commit. A commit on one of four forges is fetched as the forge's archive;
//! any other is fetched with git.

use super::fetch_git::{self, GitSource};
use super::unpack::Format;

/// Where a dependency's `url` comes from.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// An archive to download, with the format its URL names, if any.
    Archive { url: String, by_url: Option<Format> },
    /// A commit to fetch with git.
    Git(GitSource),
}

/// What a dependency's `url` names. `https://` archives are used as written,
/// without a fragment, as zig requests them. `git+https://` is a commit: on
/// codeberg.org, github.com, gitlab.com and git.sr.ht it is the forge's archive
/// of that commit, a gzipped tarball; on any other host it is fetched with git.
pub(super) fn source_of(url: &str) -> Result<Source, String> {
    if let Some(rest) = url.strip_prefix("git+https://") {
        let git = fetch_git::parse(rest, url)?;
        return Ok(match forge_archive(&git)? {
            Some(archive) => Source::Archive {
                url: archive,
                by_url: Some(Format::TarGz),
            },
            None => Source::Git(git),
        });
    }
    if !url.starts_with("https://") {
        return Err(format!("`{url}` is not an https:// or git+https:// URL"));
    }
    Ok(Source::Archive {
        url: without_fragment(url).to_string(),
        by_url: Format::of_extension(url),
    })
}

/// The URL without its fragment, which is never sent.
pub(super) fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// The forge's archive of `git`'s commit, for the four forges that have one.
/// `None` for any other host, which is fetched with git instead.
fn forge_archive(git: &GitSource) -> Result<Option<String>, String> {
    let (host, commit) = (git.host.as_str(), git.commit.as_str());
    if !matches!(
        host,
        "codeberg.org" | "github.com" | "gitlab.com" | "git.sr.ht"
    ) {
        return Ok(None);
    }
    let parts: Vec<&str> = git.path.split('/').collect();
    let [owner, repo] = parts[..] else {
        return Err(format!(
            "git dependency `{}` must be host/owner/repo",
            git.remote
        ));
    };
    if !is_name(owner) || !is_name(repo) {
        return Err(format!(
            "git dependency `{}` has a bad owner or repository",
            git.remote
        ));
    }
    Ok(match host {
        "codeberg.org" => Some(format!(
            "https://codeberg.org/{owner}/{repo}/archive/{commit}.tar.gz"
        )),
        "github.com" => Some(format!(
            "https://codeload.github.com/{owner}/{repo}/tar.gz/{commit}"
        )),
        "gitlab.com" => Some(format!(
            "https://gitlab.com/{owner}/{repo}/-/archive/{commit}/{repo}-{commit}.tar.gz"
        )),
        "git.sr.ht" if owner.starts_with('~') => Some(format!(
            "https://git.sr.ht/{owner}/{repo}/archive/{commit}.tar.gz"
        )),
        // git.sr.ht without a `~user` owner has no archive URL: git fetches it.
        _ => None,
    })
}

/// An owner or repository name: no path tricks, no odd characters.
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~'))
}
