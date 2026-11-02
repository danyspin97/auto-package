//! SourceHut (`git.sr.ht`) repositories. Their owners are `~user`, and the
//! forge's API needs a token to list tags, so the tags are read the way a
//! `git clone` reads them: `git ls-remote --tags`.

use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The SourceHut git host, matched case-insensitively.
pub(crate) fn is_sourcehut_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("git.sr.ht")
}

/// How long a repository's tag listing is reused. One package's lookup asks for
/// the listing several times; a long session must still see new tags.
const TAG_LISTING_TTL: Duration = Duration::from_secs(600);

type Listing = (Instant, Result<Vec<String>, String>);

fn listings() -> &'static Mutex<HashMap<String, Listing>> {
    static LISTINGS: OnceLock<Mutex<HashMap<String, Listing>>> = OnceLock::new();
    LISTINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The result of `fetch` for `repo_url`, reused while it is fresh. A failure is
/// kept as well, so a dead host is not asked again for every version.
fn cached_listing(
    repo_url: &str,
    fetch: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<Vec<String>, String> {
    let fresh = listings().lock().ok().and_then(|cache| {
        cache
            .get(repo_url)
            .filter(|(at, _)| at.elapsed() < TAG_LISTING_TTL)
            .map(|(_, result)| result.clone())
    });
    if let Some(result) = fresh {
        return result;
    }
    let result = fetch();
    if let Ok(mut cache) = listings().lock() {
        cache.insert(repo_url.to_string(), (Instant::now(), result.clone()));
    }
    result
}

/// The tag names of the repository at `repo_url` (a canonical https URL),
/// read with `git ls-remote`, which is bounded in time and never prompts.
pub(crate) fn ls_remote_tags(repo_url: &str) -> Result<Vec<String>, String> {
    cached_listing(repo_url, || fetch_ls_remote_tags(repo_url))
}

fn fetch_ls_remote_tags(repo_url: &str) -> Result<Vec<String>, String> {
    // The host process runs git, so the user's git configuration must not
    // rewrite the URL, supply credentials or open other transports.
    let mut env = crate::proc::minimal_env(&[]);
    for (name, value) in [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_ASKPASS", "true"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_ALLOW_PROTOCOL", "https"),
    ] {
        env.insert(name.to_string(), value.to_string());
    }
    let out = crate::proc::capture_with_timeout_env(
        "git",
        &["ls-remote", "--tags", "--", repo_url],
        crate::proc::private_dir().map_err(|e| format!("{repo_url}: {e}"))?,
        crate::proc::Emit::quiet(),
        std::time::Duration::from_secs(60),
        Some(&env),
    )
    .map_err(|e| format!("{repo_url}: git ls-remote: {e}"))?;
    if out.code != 0 {
        let stderr = out.stderr.trim();
        return Err(format!(
            "{repo_url}: git ls-remote failed: {}",
            if stderr.is_empty() {
                format!("exit {}", out.code)
            } else {
                stderr.to_string()
            }
        ));
    }
    complete_listing(repo_url, &out.stdout, crate::proc::MAX_CAPTURE_BYTES)
}

/// The tags of a listing that is complete. A capture over `cap` bytes keeps
/// only its tail, and a cut listing would lose the tags that sort first, so it
/// is refused rather than read.
fn complete_listing(repo_url: &str, output: &str, cap: u64) -> Result<Vec<String>, String> {
    if output.len() as u64 >= cap {
        return Err(format!(
            "{repo_url}: the tag listing is too large to read completely"
        ));
    }
    Ok(tags_in_ls_remote(output))
}

/// The tag names in `git ls-remote --tags` output (`<sha>\trefs/tags/<name>`),
/// without the peeled `^{}` lines of annotated tags.
pub(super) fn tags_in_ls_remote(output: &str) -> Vec<String> {
    let mut tags: Vec<String> = output
        .lines()
        .filter_map(|line| line.split_once('\t')?.1.strip_prefix("refs/tags/"))
        .filter(|name| !name.ends_with("^{}"))
        .map(str::to_string)
        .collect();
    // Newest first, as the forge APIs list them, with a tag that marks no
    // version (`latest`) after the versions; equal versions sort by name.
    let digits = |t: &str| t.chars().any(|c| c.is_ascii_digit());
    tags.sort_by(|a, b| {
        digits(b)
            .cmp(&digits(a))
            .then_with(|| version_cmp(b, a))
            .then_with(|| a.cmp(b))
    });
    tags.dedup();
    tags
}

/// The newest version among `tags`, read as [`latest_release`] reads a tag.
/// A tag with no digit (`latest`, `nightly`) marks no release, and a
/// pre-release (`v1.7-rc1`) is not the newest one.
pub(crate) fn newest_version(tags: &[String], repo: &str) -> Option<String> {
    tags.iter()
        .map(|tag| version_from_tag(tag, repo))
        .filter(|v| v.chars().any(|c| c.is_ascii_digit()) && !is_prerelease(v))
        .max_by(|a, b| version_cmp(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_remote_tags_are_the_names_without_peeled_lines() {
        let out = "91643ccf\trefs/tags/v1.5.0\n1063ce6f\trefs/tags/v1.5.0^{}\n\
                   1cb5a340\trefs/tags/v1.5.1\n1cb5a340\trefs/tags/v1.5.1\n\
                   aaaa\trefs/heads/main\n";
        assert_eq!(tags_in_ls_remote(out), vec!["v1.5.1", "v1.5.0"]);
    }

    #[test]
    fn the_newest_tag_is_the_newest_version() {
        let tags: Vec<String> = ["v1.5.0", "v1.10.0", "v1.9.0", "latest"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(newest_version(&tags, "kanshi").as_deref(), Some("1.10.0"));
        assert_eq!(newest_version(&["latest".to_string()], "kanshi"), None);
    }

    #[test]
    fn a_pre_release_tag_is_not_the_newest_release() {
        let tags = ["v1.6.0", "v1.7-rc1", "v1.7-beta"].map(String::from);
        assert_eq!(newest_version(&tags, "kanshi").as_deref(), Some("1.6.0"));
    }

    #[test]
    fn tags_are_listed_newest_first_by_version() {
        let out = "aa\trefs/tags/v1.9.0\nbb\trefs/tags/v1.10.0\ncc\trefs/tags/latest\n";
        assert_eq!(tags_in_ls_remote(out), vec!["v1.10.0", "v1.9.0", "latest"]);
    }

    #[test]
    fn a_listing_cut_short_by_the_capture_is_refused() {
        let url = "https://git.sr.ht/~o/r";
        assert!(complete_listing(url, "aa\trefs/tags/v1\n", 64).is_ok());
        assert!(complete_listing(url, "aa\trefs/tags/v1\n", 8).is_err());
    }

    #[test]
    fn a_tag_listing_is_fetched_once_per_repository() {
        let url = "https://git.sr.ht/~probe/listing-cache";
        let calls = std::cell::Cell::new(0);
        let fetch = || {
            calls.set(calls.get() + 1);
            Ok(vec!["v1.0".to_string()])
        };
        assert_eq!(cached_listing(url, fetch).unwrap(), ["v1.0"]);
        assert_eq!(cached_listing(url, fetch).unwrap(), ["v1.0"]);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn only_the_sourcehut_host_is_sourcehut() {
        assert!(is_sourcehut_host("git.sr.ht"));
        assert!(is_sourcehut_host("GIT.sr.ht"));
        assert!(!is_sourcehut_host("sr.ht.example.org"));
        assert!(!is_sourcehut_host("github.com"));
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    #[test]
    fn a_sourcehut_owner_keeps_its_tilde_and_a_download_path_is_the_repo() {
        let (host, project_path, repo) = owner_repo(
            "https://git.sr.ht/~emersion/kanshi/refs/download/v1.6.0/kanshi-1.6.0.tar.gz",
        )
        .unwrap();
        assert_eq!(host, "git.sr.ht");
        assert_eq!(project_path, "~emersion/kanshi");
        assert_eq!(repo, "kanshi");
        assert!(owner_repo("https://github.com/~emersion/kanshi").is_err());
        assert!(owner_repo("https://git.sr.ht/~/kanshi").is_err());
    }

    #[test]
    fn a_sourcehut_tag_is_archived_at_the_archive_path() {
        assert_eq!(
            tag_archive_url_for(
                "https://git.sr.ht/~emersion/kanshi",
                "kanshi",
                false,
                "v1.6.0"
            ),
            "https://git.sr.ht/~emersion/kanshi/archive/v1.6.0.tar.gz"
        );
        assert_eq!(
            tag_archive_url_for("https://github.com/o/r", "r", false, "v1.0"),
            "https://github.com/o/r/archive/refs/tags/v1.0.tar.gz"
        );
    }
}
